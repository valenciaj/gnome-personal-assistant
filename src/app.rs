//! Controlador: recibe órdenes (atajos, bandeja, popup, línea de órdenes) y coordina
//! grabación → transcripción → LLM → popup/notificación.
//!
//! Todo el estado vive en el hilo de GTK. Las peticiones HTTP y D-Bus corren en el
//! runtime de tokio y se esperan desde aquí con `runtime().spawn(...).await`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::Value;

use crate::audio::Recorder;
use crate::config::{self, AudioRetention, Config, LlmProvider, RecordingMode};
use crate::explorer::Explorer;
use crate::llm::{self, DisplayItem};
use crate::popup::Popup;
use crate::runtime::runtime;
use crate::shortcuts::Shortcuts;
use crate::store::{MessageKind, MessageRecord, Store};
use crate::tools::{self, Toolbox, TurnContext};
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
    ShowExplorer,
    ConfigureShortcuts,
    /// El detector de silencio ha decidido parar.
    AutoStop,
    RecordingFailed(String),
    /// Una tool quiere borrar algo y espera la respuesta del usuario.
    ConfirmDeletion { question: String, reply: async_channel::Sender<bool> },
    /// Mostrar y continuar una conversación del historial.
    OpenConversation(i64),
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
    /// Fila en la base de datos; se crea con el primer mensaje.
    id: Option<i64>,
    client: llm::Client,
    /// Se fija al empezar y no cambia: la API de Claude exige un prefijo idéntico
    /// en cada turno para conservar los bloques de razonamiento.
    system: String,
    /// Historial en el formato nativo del proveedor.
    messages: Vec<Value>,
}

impl Conversation {
    fn new(config: &Config) -> anyhow::Result<Self> {
        let now = chrono::Local::now().format("%A, %Y-%m-%d %H:%M (UTC%:z)");
        Ok(Self {
            id: None,
            client: llm::Client::from_config(&config.llm)?,
            system: format!("{}\n\n{}\n\nFecha y hora actuales: {now}.", config.system_prompt, tools::GUIDANCE),
            messages: Vec::new(),
        })
    }
}

/// Resultado de un turno del agente.
struct TurnOutcome {
    /// Una nota se ha quedado con el audio del mensaje.
    audio_used: bool,
    error: Option<String>,
}

pub struct Controller {
    app: adw::Application,
    commands: async_channel::Sender<Command>,
    store: Store,
    config: RefCell<Config>,
    phase: RefCell<Phase>,
    conversation: RefCell<Option<Conversation>>,
    popup: Popup,
    tray: RefCell<Option<TrayHandle>>,
    shortcuts: RefCell<Option<Arc<Shortcuts>>>,
    explorer: RefCell<Option<Rc<Explorer>>>,
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
        let store = Store::open(&config::data_dir().join("assistant.db")).unwrap_or_else(|err| {
            log::error!("{err:#}; los datos de esta sesión no se guardarán");
            Store::in_memory().expect("SQLite en memoria")
        });
        let this = Rc::new(Self {
            app: app.clone(),
            popup: Popup::new(app, commands.clone()),
            commands,
            store,
            config: RefCell::new(config),
            phase: RefCell::new(Phase::Idle),
            conversation: RefCell::new(None),
            tray: RefCell::new(None),
            shortcuts: RefCell::new(None),
            explorer: RefCell::new(None),
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
        if crate::dev::enabled() {
            log::info!("Modo desarrollo: sin bandeja ni atajos");
        } else {
            glib::spawn_future_local(glib::clone!(
                #[strong]
                this,
                async move { this.connect_services().await }
            ));
        }
        if let Some(dir) = crate::dev::snapshot_dir() {
            this.take_snapshots(dir);
        }

        this.clean_audio();
        let weak = Rc::downgrade(&this);
        glib::timeout_add_seconds_local(6 * 3600, move || match weak.upgrade() {
            Some(this) => {
                this.clean_audio();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
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
                        async move { this.ask(text, None).await }
                    ));
                }
            }
            Command::ConfirmDeletion { question, reply } => {
                self.present_popup();
                self.popup.ask_confirmation(&question, reply);
            }
            Command::OpenConversation(id) => {
                if matches!(*self.phase.borrow(), Phase::Idle) {
                    self.reload_config();
                    if let Err(err) = self.open_conversation(id) {
                        self.fail(&format!("{err:#}"));
                    }
                }
            }
            Command::NewTextConversation => {
                if matches!(*self.phase.borrow(), Phase::Idle) {
                    *self.conversation.borrow_mut() = None;
                    self.popup.view().clear();
                }
                self.present_popup();
            }
            Command::Reopen => self.present_popup(),
            Command::ShowExplorer => {
                let explorer = self
                    .explorer
                    .borrow_mut()
                    .get_or_insert_with(|| Explorer::new(&self.app, self.store.clone(), self.commands.clone()))
                    .clone();
                explorer.present();
            }
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
        let path = config::audio_dir().join(format!("rec-{}.ogg", chrono::Local::now().format("%Y%m%d-%H%M%S")));
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
            self.popup.view().clear();
        }
        self.present_popup();
        self.popup.set_status(Some("Transcribiendo…"));

        glib::spawn_future_local(glib::clone!(
            #[strong(rename_to = this)]
            self,
            async move {
                match this.transcribe(recording.recorder).await {
                    Ok((text, path)) if text.is_empty() => {
                        let _ = std::fs::remove_file(path);
                        this.fail("No se ha detectado voz en la grabación");
                    }
                    Ok((text, path)) => this.ask(text, Some(path)).await,
                    Err(err) => this.fail(&format!("{err:#}")),
                }
            }
        ));
    }

    /// Devuelve el texto y la ruta del audio, que se conserva según la retención.
    async fn transcribe(&self, recorder: Recorder) -> anyhow::Result<(String, PathBuf)> {
        let path = recorder.stop().await?;
        let stt = self.config.borrow().stt.clone();
        let result = runtime()
            .spawn({
                let path = path.clone();
                async move { crate::stt::transcribe(&stt, &path).await }
            })
            .await?;
        match result {
            Ok(text) => Ok((text, path)),
            Err(err) => {
                let _ = std::fs::remove_file(&path);
                Err(err)
            }
        }
    }

    /// Envía un mensaje del usuario (con su audio, si lo dictó) y muestra la respuesta.
    async fn ask(self: Rc<Self>, text: String, audio: Option<PathBuf>) {
        *self.phase.borrow_mut() = Phase::Processing;
        self.set_tray(TrayState::Busy);
        let config = self.config.borrow().clone();
        let kept_audio = audio
            .as_ref()
            .filter(|_| config.audio.retention == AudioRetention::Always)
            .map(|p| p.to_string_lossy().into_owned());
        self.popup.view().add_user(&text, kept_audio.as_deref());
        self.popup.set_status(Some("Pensando…"));

        let outcome = self.run_turn(&config, &text, audio.as_deref()).await;
        let outcome = outcome.unwrap_or_else(|err| TurnOutcome { audio_used: false, error: Some(format!("{err:#}")) });

        if let Some(path) = &audio {
            let keep = outcome.audio_used || config.audio.retention == AudioRetention::Always;
            if !keep {
                let _ = std::fs::remove_file(path);
            }
        }
        match outcome.error {
            None => self.finish(TrayState::Idle),
            Some(error) => self.fail(&error),
        }
        // La IA puede haber cambiado notas o tareas.
        if let Some(explorer) = self.explorer.borrow().clone() {
            explorer.refresh();
        }
    }

    async fn run_turn(&self, config: &Config, text: &str, audio: Option<&Path>) -> anyhow::Result<TurnOutcome> {
        let mut conversation = match self.conversation.borrow_mut().take() {
            Some(conversation) => conversation,
            None => Conversation::new(config)?,
        };
        let outcome = self.run_turn_in(&mut conversation, config, text, audio).await;
        *self.conversation.borrow_mut() = Some(conversation);
        outcome
    }

    async fn run_turn_in(
        &self,
        conversation: &mut Conversation,
        config: &Config,
        text: &str,
        audio: Option<&Path>,
    ) -> anyhow::Result<TurnOutcome> {
        let audio = audio.map(|p| p.to_string_lossy().into_owned());
        let id = match conversation.id {
            Some(id) => id,
            None => {
                let client = &conversation.client;
                let id = self.store.create_conversation(text, client.provider.id(), &client.model, &conversation.system)?;
                conversation.id = Some(id);
                id
            }
        };
        let mut user = MessageRecord::new(MessageKind::User, text);
        if config.audio.retention == AudioRetention::Always {
            user.audio_path = audio.clone();
        }
        self.store.add_message(id, &user)?;

        let toolbox = Toolbox::new(
            self.store.clone(),
            self.commands.clone(),
            TurnContext {
                conversation_id: Some(id),
                transcript: audio.is_some().then(|| text.to_owned()),
                audio_path: audio,
            },
        );
        let client = conversation.client.clone();
        let system = conversation.system.clone();
        // Se trabaja sobre una copia: si el turno falla, el historial queda como estaba.
        let mut messages = conversation.messages.clone();
        messages.push(client.user_message(text));
        let (messages, items, audio_used, result) = runtime()
            .spawn(async move {
                let mut items = Vec::new();
                let result =
                    llm::run_agent(&client, &system, &mut messages, &tools::specs(), &toolbox, &mut items).await;
                (messages, items, toolbox.audio_used(), result)
            })
            .await?;

        for item in &items {
            self.store.add_message(id, &record_for(item))?;
        }
        self.popup.view().add_items(&items);

        let error = match result {
            Ok(()) => {
                conversation.messages = messages;
                self.store.save_native(id, &conversation.messages)?;
                if let Some(DisplayItem::Text(answer)) = items.iter().rev().find(|i| matches!(i, DisplayItem::Text(_))) {
                    self.notify("Asistente", answer);
                }
                None
            }
            Err(err) => {
                let message = format!("{err:#}");
                self.store.add_message(id, &MessageRecord::new(MessageKind::Error, &message))?;
                Some(message)
            }
        };
        Ok(TurnOutcome { audio_used, error })
    }

    /// Carga una conversación guardada en el popup para seguirla.
    fn open_conversation(&self, id: i64) -> anyhow::Result<()> {
        let info = self.store.get_conversation(id)?;
        let records = self.store.messages(id)?;
        self.popup.view().clear();
        for record in &records {
            self.popup.view().add_record(record);
        }
        self.present_popup();

        let provider = LlmProvider::from_id(&info.provider)
            .ok_or_else(|| anyhow::anyhow!("Proveedor desconocido: {}", info.provider))?;
        // Se continúa con el proveedor y el modelo con los que empezó.
        let client = llm::Client::new(&self.config.borrow().llm, provider, Some(&info.model))?;
        *self.conversation.borrow_mut() =
            Some(Conversation { id: Some(id), client, system: info.system, messages: info.native });
        Ok(())
    }

    /// Capturas del explorador y del popup para revisar la interfaz (ver `dev`).
    fn take_snapshots(self: &Rc<Self>, dir: PathBuf) {
        let _ = std::fs::create_dir_all(&dir);
        self.handle(Command::ShowExplorer);
        let explorer = self.explorer.borrow().clone().expect("explorador creado");
        let mut steps: Vec<Box<dyn FnOnce()>> = Vec::new();
        for page in ["notes", "todos", "conversations"] {
            let shown = explorer.clone();
            steps.push(Box::new(move || shown.show_page(page)));
            let (explorer, path) = (explorer.clone(), dir.join(format!("{page}.png")));
            steps.push(Box::new(move || log_snapshot(crate::dev::save_png(explorer.window(), &path), &path)));
        }
        let editing = explorer.clone();
        steps.push(Box::new(move || {
            editing.show_page("notes");
            editing.edit_selected_note();
        }));
        let path = dir.join("notes-edit.png");
        steps.push(Box::new(move || log_snapshot(crate::dev::save_png(explorer.window(), &path), &path)));
        let this = self.clone();
        steps.push(Box::new(move || {
            if let Some(c) = this.store.list_conversations(1).ok().and_then(|c| c.into_iter().next()) {
                this.handle(Command::OpenConversation(c.id));
            }
        }));
        let (this, path) = (self.clone(), dir.join("popup.png"));
        steps.push(Box::new(move || log_snapshot(crate::dev::save_png(this.popup.window(), &path), &path)));
        let app = self.app.clone();
        steps.push(Box::new(move || app.quit()));
        crate::dev::run_steps(Duration::from_millis(1200), steps);
    }

    /// Borra los audios que no se van a usar: huérfanos de más de una hora y, si se
    /// ha configurado, los del historial con más de N días. Los de notas nunca.
    fn clean_audio(&self) {
        let days = self.config.borrow().audio.delete_after_days;
        let store = self.store.clone();
        runtime().spawn_blocking(move || {
            if let Err(err) = clean_audio(&store, days) {
                log::warn!("Limpieza de audios: {err:#}");
            }
        });
    }

    fn fail(&self, message: &str) {
        log::error!("{message}");
        self.popup.view().add_error(message);
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

fn record_for(item: &DisplayItem) -> MessageRecord {
    match item {
        DisplayItem::Reasoning(text) => MessageRecord::new(MessageKind::Reasoning, text),
        DisplayItem::Text(text) => MessageRecord::new(MessageKind::Text, text),
        DisplayItem::Tool(result) => MessageRecord {
            tool_name: Some(result.call.name.clone()),
            tool_input: Some(result.call.input.to_string()),
            tool_output: Some(result.content.clone()),
            is_error: result.is_error,
            ..MessageRecord::new(MessageKind::Tool, &result.summary)
        },
    }
}

fn clean_audio(store: &Store, days: u32) -> anyhow::Result<()> {
    let dir = config::audio_dir();
    if !dir.exists() {
        return Ok(());
    }
    let in_notes = store.note_audio_paths()?;
    let referenced = store.referenced_audio_paths()?;
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let key = path.to_string_lossy().into_owned();
        if in_notes.contains(&key) {
            continue;
        }
        let age = path.metadata()?.modified()?.elapsed().unwrap_or_default();
        let orphan = !referenced.contains(&key) && age > Duration::from_secs(3600);
        let expired = days > 0 && age > Duration::from_secs(u64::from(days) * 86_400);
        if orphan || expired {
            log::info!("Borrando audio {}", path.display());
            std::fs::remove_file(&path)?;
            store.forget_message_audio(&key)?;
        }
    }
    Ok(())
}

fn log_snapshot(result: anyhow::Result<()>, path: &Path) {
    match result {
        Ok(()) => log::info!("Captura guardada en {}", path.display()),
        Err(err) => log::error!("Captura {}: {err:#}", path.display()),
    }
}
