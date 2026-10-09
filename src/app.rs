//! Controlador: recibe órdenes (atajos, bandeja, popup, línea de órdenes) y coordina
//! grabación → transcripción → LLM → popup/notificación.
//!
//! Todo el estado vive en el hilo de GTK. Las peticiones HTTP y D-Bus corren en el
//! runtime de tokio y se esperan desde aquí con `runtime().spawn(...).await`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::Value;

use crate::audio::Recorder;
use crate::config::{self, Config, RecordingMode};
use crate::llm::{self, DisplayItem};
use crate::popup::Popup;
use crate::runtime::runtime;
use crate::shortcuts::Shortcuts;
use crate::tray::{self, TrayHandle, TrayState};

#[derive(Debug, Clone)]
pub enum Command {
    /// Atajo de grabación pulsado (con el token para dar foco a la ventana).
    ShortcutPressed { token: Option<String> },
    /// Atajo de grabación soltado (modo mantener pulsado).
    ShortcutReleased,
    /// Bandeja / línea de órdenes: grabar en una conversación nueva.
    ToggleRecording,
    /// Botón del micrófono del popup: grabar en la conversación abierta.
    TogglePopupRecording,
    /// Texto escrito en el popup.
    SendText(String),
    NewTextConversation,
    Reopen,
    ConfigureShortcuts,
    /// El detector de silencio ha decidido parar.
    AutoStop,
    RecordingFailed(String),
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    NewConversation,
    CurrentConversation,
}

struct Recording {
    recorder: Recorder,
    target: Target,
    id: u64,
    speech_heard: bool,
    silence_since: Option<Instant>,
    stop_requested: bool,
}

enum Phase {
    Idle,
    Recording(Recording),
    Processing,
}

struct Conversation {
    client: llm::Client,
    /// Se fija al empezar y no cambia: la API de Claude exige un prefijo idéntico
    /// en cada turno para conservar los bloques de razonamiento.
    system: String,
    messages: Vec<Value>,
}

impl Conversation {
    fn new(config: &Config) -> anyhow::Result<Self> {
        let now = chrono::Local::now().format("%A, %Y-%m-%d %H:%M (UTC%:z)");
        Ok(Self {
            client: llm::Client::from_config(&config.llm)?,
            system: format!("{}\n\nFecha y hora actuales: {now}.", config.system_prompt),
            messages: Vec::new(),
        })
    }
}

pub struct Controller {
    app: adw::Application,
    commands: async_channel::Sender<Command>,
    config: RefCell<Config>,
    phase: RefCell<Phase>,
    conversation: RefCell<Option<Conversation>>,
    popup: Popup,
    tray: RefCell<Option<TrayHandle>>,
    shortcuts: RefCell<Option<Arc<Shortcuts>>>,
    activation_token: RefCell<Option<String>>,
    next_recording_id: RefCell<u64>,
}

impl Controller {
    pub fn start(
        app: &adw::Application,
        commands: async_channel::Sender<Command>,
        receiver: async_channel::Receiver<Command>,
    ) -> Rc<Self> {
        let config = Config::load().unwrap_or_else(|err| {
            log::error!("{err:#}");
            Config::default()
        });
        let this = Rc::new(Self {
            app: app.clone(),
            popup: Popup::new(app, commands.clone()),
            commands,
            config: RefCell::new(config),
            phase: RefCell::new(Phase::Idle),
            conversation: RefCell::new(None),
            tray: RefCell::new(None),
            shortcuts: RefCell::new(None),
            activation_token: RefCell::new(None),
            next_recording_id: RefCell::new(0),
        });

        glib::spawn_future_local(glib::clone!(
            #[strong]
            this,
            async move {
                while let Ok(command) = receiver.recv().await {
                    this.handle(command);
                }
            }
        ));
        glib::spawn_future_local(glib::clone!(
            #[strong]
            this,
            async move { this.connect_services().await }
        ));
        this
    }

    async fn connect_services(self: Rc<Self>) {
        match runtime().spawn(tray::spawn(self.commands.clone())).await {
            Ok(Ok(handle)) => *self.tray.borrow_mut() = Some(handle),
            Ok(Err(err)) => log::error!("No se pudo crear el icono de bandeja: {err:#}"),
            Err(err) => log::error!("{err}"),
        }
        match runtime().spawn(Shortcuts::register(self.commands.clone())).await {
            Ok(Ok(shortcuts)) => *self.shortcuts.borrow_mut() = Some(Arc::new(shortcuts)),
            Ok(Err(err)) => {
                log::error!("Atajos globales no disponibles: {err:#}");
                self.notify(
                    "Atajos de teclado no disponibles",
                    &format!("{err:#}. Puedes usar «gnome-personal-assistant toggle» en un atajo personalizado de GNOME."),
                );
            }
            Err(err) => log::error!("{err}"),
        }
    }

    fn handle(self: &Rc<Self>, command: Command) {
        log::debug!("Orden: {command:?}");
        match command {
            Command::ShortcutPressed { token } => {
                *self.activation_token.borrow_mut() = token;
                let recording = matches!(*self.phase.borrow(), Phase::Recording(_));
                let mode = self.config.borrow().recording.mode;
                if !(mode == RecordingMode::PushToTalk && recording) {
                    self.toggle_recording(Target::NewConversation);
                }
            }
            Command::ShortcutReleased => {
                let mode = self.config.borrow().recording.mode;
                if mode == RecordingMode::PushToTalk {
                    self.stop_recording();
                }
            }
            Command::ToggleRecording => self.toggle_recording(Target::NewConversation),
            Command::TogglePopupRecording => self.toggle_recording(Target::CurrentConversation),
            Command::AutoStop => self.stop_recording(),
            Command::RecordingFailed(message) => {
                if let Phase::Recording(recording) = self.phase.replace(Phase::Idle) {
                    recording.recorder.cancel();
                }
                self.popup.set_recording(false);
                self.fail(&message);
            }
            Command::SendText(text) => {
                if matches!(*self.phase.borrow(), Phase::Idle) {
                    self.reload_config();
                    glib::spawn_future_local(glib::clone!(
                        #[strong(rename_to = this)]
                        self,
                        async move { this.ask(text).await }
                    ));
                }
            }
            Command::NewTextConversation => {
                if matches!(*self.phase.borrow(), Phase::Idle) {
                    *self.conversation.borrow_mut() = None;
                    self.popup.clear();
                }
                self.present_popup();
            }
            Command::Reopen => self.present_popup(),
            Command::ConfigureShortcuts => {
                if let Some(shortcuts) = self.shortcuts.borrow().clone() {
                    runtime().spawn(async move {
                        if let Err(err) = shortcuts.configure().await {
                            log::error!("No se pudo abrir la configuración de atajos: {err:#}");
                        }
                    });
                }
            }
            Command::Quit => self.app.quit(),
        }
    }

    fn reload_config(&self) {
        match Config::load() {
            Ok(config) => *self.config.borrow_mut() = config,
            Err(err) => log::error!("{err:#}; se mantiene la configuración anterior"),
        }
    }

    fn toggle_recording(self: &Rc<Self>, target: Target) {
        let idle = match *self.phase.borrow() {
            Phase::Idle => true,
            Phase::Recording(_) => false,
            Phase::Processing => {
                self.present_popup();
                return;
            }
        };
        if idle {
            self.start_recording(target);
        } else {
            self.stop_recording();
        }
    }

    fn start_recording(self: &Rc<Self>, target: Target) {
        self.reload_config();
        let id = {
            let mut next = self.next_recording_id.borrow_mut();
            *next += 1;
            *next
        };
        let path = config::cache_dir().join(format!("rec-{}.ogg", chrono::Local::now().format("%Y%m%d-%H%M%S")));
        let weak = Rc::downgrade(self);
        let errors = self.commands.clone();
        let recorder = Recorder::start(
            &path,
            move |db| {
                if let Some(this) = weak.upgrade() {
                    this.on_level(db);
                }
            },
            move |message| {
                let _ = errors.try_send(Command::RecordingFailed(message));
            },
        );
        let recorder = match recorder {
            Ok(recorder) => recorder,
            Err(err) => return self.fail(&format!("{err:#}")),
        };

        *self.phase.borrow_mut() = Phase::Recording(Recording {
            recorder,
            target,
            id,
            speech_heard: false,
            silence_since: None,
            stop_requested: false,
        });
        self.set_tray(TrayState::Recording);
        self.popup.set_recording(true);

        // Límite de duración en cualquier modo.
        let max = Duration::from_secs(self.config.borrow().recording.max_seconds.max(1).into());
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(max, move || {
            if let Some(this) = weak.upgrade()
                && matches!(&*this.phase.borrow(), Phase::Recording(r) if r.id == id)
            {
                let _ = this.commands.try_send(Command::AutoStop);
            }
        });
    }

    /// Detector de silencio del modo auto_stop.
    fn on_level(&self, db: f64) {
        let (mode, threshold, silence) = {
            let config = self.config.borrow();
            let r = &config.recording;
            (r.mode, r.voice_threshold_db, Duration::from_secs_f64(r.silence_seconds.max(0.3)))
        };
        if mode != RecordingMode::AutoStop {
            return;
        }
        let mut phase = self.phase.borrow_mut();
        let Phase::Recording(recording) = &mut *phase else { return };
        if recording.stop_requested {
            return;
        }
        if db > threshold {
            recording.speech_heard = true;
            recording.silence_since = None;
        } else if recording.speech_heard {
            let since = *recording.silence_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= silence {
                recording.stop_requested = true;
                let _ = self.commands.try_send(Command::AutoStop);
            }
        }
    }

    fn stop_recording(self: &Rc<Self>) {
        if !matches!(*self.phase.borrow(), Phase::Recording(_)) {
            return;
        }
        let Phase::Recording(recording) = self.phase.replace(Phase::Processing) else {
            unreachable!()
        };
        self.popup.set_recording(false);

        // Pulsaciones accidentales en modo mantener pulsado.
        if recording.recorder.elapsed() < Duration::from_millis(400) {
            recording.recorder.cancel();
            *self.phase.borrow_mut() = Phase::Idle;
            self.set_tray(TrayState::Idle);
            return;
        }

        self.set_tray(TrayState::Busy);
        if recording.target == Target::NewConversation {
            *self.conversation.borrow_mut() = None;
            self.popup.clear();
        }
        self.present_popup();
        self.popup.set_status(Some("Transcribiendo…"));

        glib::spawn_future_local(glib::clone!(
            #[strong(rename_to = this)]
            self,
            async move {
                match this.transcribe(recording.recorder).await {
                    Ok(text) if text.is_empty() => this.fail("No se ha detectado voz en la grabación"),
                    Ok(text) => this.ask(text).await,
                    Err(err) => this.fail(&format!("{err:#}")),
                }
            }
        ));
    }

    async fn transcribe(&self, recorder: Recorder) -> anyhow::Result<String> {
        let path = recorder.stop().await?;
        let stt = self.config.borrow().stt.clone();
        let result = runtime()
            .spawn({
                let path = path.clone();
                async move { crate::stt::transcribe(&stt, &path).await }
            })
            .await?;
        // Fase 2: según la retención configurada, el audio se conservará con la nota.
        let _ = std::fs::remove_file(&path);
        result
    }

    /// Envía un mensaje del usuario y muestra la respuesta.
    async fn ask(self: Rc<Self>, text: String) {
        *self.phase.borrow_mut() = Phase::Processing;
        self.set_tray(TrayState::Busy);
        self.popup.add_user(&text);
        self.popup.set_status(Some("Pensando…"));

        let conversation = match self.conversation.borrow_mut().take() {
            Some(conversation) => Ok(conversation),
            None => Conversation::new(&self.config.borrow()),
        };
        let mut conversation = match conversation {
            Ok(conversation) => conversation,
            Err(err) => return self.fail(&format!("{err:#}")),
        };

        let previous_len = conversation.messages.len();
        conversation.messages.push(conversation.client.user_message(&text));
        let outcome = runtime()
            .spawn(async move {
                let result = llm::run_agent(
                    &conversation.client,
                    &conversation.system,
                    &mut conversation.messages,
                    &[],
                    &llm::NoTools,
                )
                .await;
                (conversation, result)
            })
            .await;

        let (mut conversation, result) = match outcome {
            Ok(outcome) => outcome,
            Err(err) => return self.fail(&format!("Error interno: {err}")),
        };
        match result {
            Ok(items) => {
                self.popup.add_items(&items);
                if let Some(DisplayItem::Text(answer)) =
                    items.iter().rev().find(|i| matches!(i, DisplayItem::Text(_)))
                {
                    self.notify("Asistente", answer);
                }
                *self.conversation.borrow_mut() = Some(conversation);
                self.finish(TrayState::Idle);
            }
            Err(err) => {
                // Se descarta el turno fallido para poder reintentar sobre un historial válido.
                conversation.messages.truncate(previous_len);
                *self.conversation.borrow_mut() = Some(conversation);
                self.fail(&format!("{err:#}"));
            }
        }
    }

    fn fail(&self, message: &str) {
        log::error!("{message}");
        self.popup.add_error(message);
        self.notify("Error del asistente", message);
        self.finish(TrayState::Error);
    }

    fn finish(&self, tray: TrayState) {
        *self.phase.borrow_mut() = Phase::Idle;
        self.popup.set_status(None);
        self.set_tray(tray);
    }

    fn present_popup(&self) {
        let token = self.activation_token.borrow_mut().take();
        self.popup.present(token.as_deref());
    }

    fn set_tray(&self, state: TrayState) {
        if let Some(handle) = self.tray.borrow().clone() {
            runtime().spawn(async move { tray::set_state(&handle, state).await });
        }
    }

    fn notify(&self, title: &str, body: &str) {
        if self.popup.is_active() {
            return;
        }
        let notification = gio::Notification::new(title);
        let body: String = body.chars().take(300).collect();
        notification.set_body(Some(&body));
        notification.set_default_action("app.reopen");
        self.app.send_notification(Some("assistant"), &notification);
    }
}
