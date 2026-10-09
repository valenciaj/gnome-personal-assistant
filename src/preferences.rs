//! Ventana de preferencias. Los cambios se guardan en config.toml al cerrarla; las
//! API keys van al llavero en cuanto se aplican.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::app::Command;
use crate::config::{self, AudioRetention, Config, LlmProvider, RecordingMode, STT_SECRET, SttProvider};
use crate::runtime::runtime;
use crate::{audio, llm, secrets};

const STT_MODELS: [&str; 3] = ["gpt-4o-transcribe", "gpt-4o-mini-transcribe", "whisper-1"];
const RECORDING_MODES: [(RecordingMode, &str); 3] = [
    (RecordingMode::Toggle, "Pulsar para empezar y para parar"),
    (RecordingMode::PushToTalk, "Mantener pulsado el atajo"),
    (RecordingMode::AutoStop, "Parar sola al detectar silencio"),
];

struct Prefs {
    dialog: adw::PreferencesDialog,
    config: RefCell<Config>,
    loading: Cell<bool>,
    // IA
    provider: adw::ComboRow,
    url: adw::EntryRow,
    key: adw::PasswordEntryRow,
    model: adw::EntryRow,
    models_list: gtk::ListBox,
    models_status: gtk::Label,
    reasoning: adw::ComboRow,
}

pub fn show(parent: Option<&gtk::Widget>, commands: async_channel::Sender<Command>) -> adw::PreferencesDialog {
    let config = Config::load().unwrap_or_else(|err| {
        log::error!("{err:#}");
        Config::default()
    });
    let dialog = adw::PreferencesDialog::builder().title("Preferencias").search_enabled(true).build();

    let models_list = gtk::ListBox::builder().css_classes(["navigation-sidebar"]).build();
    let models_status = gtk::Label::builder().wrap(true).max_width_chars(30).margin_top(6).margin_bottom(6).css_classes(["dim-label"]).build();
    let prefs = Rc::new(Prefs {
        dialog: dialog.clone(),
        config: RefCell::new(config),
        loading: Cell::new(false),
        provider: adw::ComboRow::builder()
            .title("Proveedor")
            .model(&gtk::StringList::new(&LlmProvider::ALL.map(LlmProvider::label)))
            .build(),
        url: adw::EntryRow::builder().title("URL base").build(),
        key: adw::PasswordEntryRow::builder().title("API key").show_apply_button(true).build(),
        model: adw::EntryRow::builder().title("Modelo").build(),
        models_list,
        models_status,
        reasoning: adw::ComboRow::builder().title("Razonamiento").build(),
    });

    dialog.add(&general_page(&prefs, &commands));
    dialog.add(&ai_page(&prefs));
    dialog.add(&recording_page(&prefs));
    dialog.add(&data_page(&prefs));

    dialog.connect_closed(glib::clone!(
        #[strong]
        prefs,
        move |_| {
            match prefs.config.borrow().save() {
                Ok(()) => {
                    let _ = commands.try_send(Command::ConfigChanged);
                }
                Err(err) => log::error!("No se pudo guardar la configuración: {err:#}"),
            }
        }
    ));
    dialog.present(parent);
    dialog
}

impl Prefs {
    fn toast(&self, message: &str) {
        self.dialog.add_toast(adw::Toast::new(message));
    }

    fn provider(&self) -> LlmProvider {
        self.config.borrow().llm.provider
    }

    /// Carga en las filas los ajustes del proveedor elegido.
    fn load_provider(&self) {
        self.loading.set(true);
        let config = self.config.borrow();
        let provider = config.llm.provider;
        let settings = config.llm.settings(provider);
        self.provider.set_selected(LlmProvider::ALL.iter().position(|p| *p == provider).unwrap_or(0) as u32);
        self.url.set_text(&settings.base_url);
        self.key.set_text(&secrets::get(&provider.secret_name()).unwrap_or_default());
        self.key.set_visible(provider != LlmProvider::Ollama);
        self.model.set_text(&settings.model);
        let levels = provider.reasoning_levels();
        self.reasoning.set_model(Some(&gtk::StringList::new(&levels.iter().map(|(_, label)| *label).collect::<Vec<_>>())));
        self.reasoning.set_selected(levels.iter().position(|(value, _)| *value == settings.reasoning).unwrap_or(0) as u32);
        self.models_list.remove_all();
        self.models_status.set_text("");
        self.loading.set(false);
    }

    /// Pide al proveedor su lista de modelos y la muestra en el desplegable.
    async fn fetch_models(self: Rc<Self>) {
        self.models_list.remove_all();
        self.models_status.set_text("Consultando modelos…");
        self.models_status.set_visible(true);
        let provider = self.provider();
        // Aplicar una clave escrita pero aún no guardada.
        let typed = self.key.text().to_string();
        if provider != LlmProvider::Ollama && secrets::get(&provider.secret_name()).unwrap_or_default() != typed {
            save_secret(&self, provider.secret_name(), typed).await;
        }
        // Para listar da igual el modelo, pero el cliente exige uno.
        let client = llm::Client::new(&self.config.borrow().llm, provider, Some("-"));
        let result = match client {
            Ok(client) => runtime().spawn(async move { client.list_models().await }).await.map_err(anyhow::Error::from).and_then(|r| r),
            Err(err) => Err(err),
        };
        match result {
            Ok(models) if models.is_empty() => self.models_status.set_text("El proveedor no ha devuelto modelos"),
            Ok(models) => {
                self.models_status.set_visible(false);
                for model in models {
                    let label = gtk::Label::builder().label(&model).xalign(0.0).build();
                    self.models_list.append(&label);
                }
            }
            Err(err) => self.models_status.set_text(&format!("{err:#}")),
        }
    }
}

async fn save_secret(prefs: &Prefs, name: String, value: String) {
    let saved = runtime().spawn(async move { secrets::set(&name, &value).await }).await;
    match saved {
        Ok(Ok(())) => prefs.toast("Clave guardada en el llavero"),
        Ok(Err(err)) => prefs.toast(&format!("No se pudo guardar la clave: {err:#}")),
        Err(err) => prefs.toast(&format!("{err}")),
    }
}

fn general_page(prefs: &Rc<Prefs>, commands: &async_channel::Sender<Command>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder().name("general").title("General").icon_name("emblem-system-symbolic").build();

    let prompt_group = adw::PreferencesGroup::builder()
        .title("Instrucciones del asistente")
        .description("Cómo debe comportarse. Se aplican a las conversaciones nuevas.")
        .build();
    let buffer = gtk::TextBuffer::new(None);
    buffer.set_text(&prefs.config.borrow().system_prompt);
    let text = gtk::TextView::builder()
        .buffer(&buffer)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(8)
        .right_margin(8)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .min_content_height(140)
        .child(&text)
        .css_classes(["card"])
        .build();
    prompt_group.add(&scroller);
    buffer.connect_changed(glib::clone!(
        #[weak]
        prefs,
        move |buffer| {
            let (start, end) = buffer.bounds();
            prefs.config.borrow_mut().system_prompt = buffer.text(&start, &end, false).to_string();
        }
    ));
    let restore = gtk::Button::builder()
        .label("Restaurar las instrucciones por defecto")
        .halign(gtk::Align::End)
        .margin_top(6)
        .css_classes(["flat"])
        .build();
    restore.connect_clicked(move |_| buffer.set_text(config::DEFAULT_SYSTEM_PROMPT));
    prompt_group.add(&restore);
    page.add(&prompt_group);

    let shortcuts = adw::PreferencesGroup::builder().title("Atajos de teclado").build();
    let row = adw::ActionRow::builder()
        .title("Atajos globales")
        .subtitle("Por defecto Super+Alt+Espacio graba y Super+Alt+R reabre la última conversación")
        .build();
    let change = gtk::Button::builder().label("Cambiar…").valign(gtk::Align::Center).build();
    let commands = commands.clone();
    change.connect_clicked(move |_| {
        let _ = commands.try_send(Command::ConfigureShortcuts);
    });
    row.add_suffix(&change);
    shortcuts.add(&row);
    page.add(&shortcuts);
    page
}

fn ai_page(prefs: &Rc<Prefs>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder().name("ai").title("IA").icon_name("starred-symbolic").build();

    // ── Modelo de lenguaje ──
    let group = adw::PreferencesGroup::builder()
        .title("Modelo de lenguaje")
        .description("El proveedor y el modelo que responden. Las conversaciones ya empezadas siguen con su modelo.")
        .build();
    let models_popover = gtk::Popover::builder()
        .child(&{
            let scroller = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .max_content_height(320)
                .propagate_natural_height(true)
                .min_content_width(260)
                .child(&prefs.models_list)
                .build();
            let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
            content.append(&prefs.models_status);
            content.append(&scroller);
            content
        })
        .build();
    let models_button = gtk::MenuButton::builder()
        .icon_name("pan-down-symbolic")
        .tooltip_text("Elegir de los modelos disponibles")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .popover(&models_popover)
        .build();
    prefs.model.add_suffix(&models_button);
    let test = adw::ButtonRow::builder().title("Comprobar la conexión").build();

    for row in [
        prefs.provider.upcast_ref::<gtk::Widget>(),
        prefs.model.upcast_ref(),
        prefs.reasoning.upcast_ref(),
        prefs.key.upcast_ref(),
        prefs.url.upcast_ref(),
        test.upcast_ref(),
    ] {
        group.add(row);
    }
    page.add(&group);
    prefs.load_provider();

    prefs.provider.connect_selected_notify(glib::clone!(
        #[weak]
        prefs,
        move |row| {
            if prefs.loading.get() {
                return;
            }
            prefs.config.borrow_mut().llm.provider = LlmProvider::ALL[row.selected() as usize];
            prefs.load_provider();
        }
    ));
    prefs.url.connect_changed(glib::clone!(
        #[weak]
        prefs,
        move |row| {
            if !prefs.loading.get() {
                let provider = prefs.provider();
                prefs.config.borrow_mut().llm.settings_mut(provider).base_url = row.text().trim().to_owned();
            }
        }
    ));
    prefs.model.connect_changed(glib::clone!(
        #[weak]
        prefs,
        move |row| {
            if !prefs.loading.get() {
                let provider = prefs.provider();
                prefs.config.borrow_mut().llm.settings_mut(provider).model = row.text().trim().to_owned();
            }
        }
    ));
    prefs.reasoning.connect_selected_notify(glib::clone!(
        #[weak]
        prefs,
        move |row| {
            if prefs.loading.get() {
                return;
            }
            let provider = prefs.provider();
            if let Some((value, _)) = provider.reasoning_levels().get(row.selected() as usize) {
                prefs.config.borrow_mut().llm.settings_mut(provider).reasoning = (*value).to_owned();
            }
        }
    ));
    prefs.key.connect_apply(glib::clone!(
        #[weak]
        prefs,
        move |row| {
            let (name, value) = (prefs.provider().secret_name(), row.text().trim().to_owned());
            glib::spawn_future_local(async move { save_secret(&prefs, name, value).await });
        }
    ));
    models_popover.connect_show(glib::clone!(
        #[weak]
        prefs,
        move |_| {
            glib::spawn_future_local(prefs.fetch_models());
        }
    ));
    prefs.models_list.connect_row_activated(glib::clone!(
        #[weak]
        prefs,
        #[weak]
        models_popover,
        move |_, row| {
            if let Some(label) = row.child().and_downcast::<gtk::Label>() {
                prefs.model.set_text(&label.text());
            }
            models_popover.popdown();
        }
    ));
    test.connect_activated(glib::clone!(
        #[weak]
        prefs,
        move |_| {
            glib::spawn_future_local(async move {
                prefs.clone().fetch_models().await;
                let count = prefs.models_list.observe_children().n_items();
                if count > 0 {
                    prefs.toast(&format!("Conexión correcta: {count} modelos disponibles"));
                } else {
                    prefs.toast(&prefs.models_status.text());
                }
            });
        }
    ));

    // ── Transcripción ──
    let stt = adw::PreferencesGroup::builder()
        .title("Transcripción")
        .description("Convierte tu voz en texto. Si no pones una clave propia se usa la de OpenAI de arriba.")
        .build();
    let stt_config = prefs.config.borrow().stt.clone();
    let stt_provider = adw::ComboRow::builder()
        .title("Servicio")
        .model(&gtk::StringList::new(&["OpenAI", "Compatible con OpenAI"]))
        .selected(match stt_config.provider {
            SttProvider::Openai => 0,
            SttProvider::OpenaiCompatible => 1,
        })
        .build();
    let stt_model = adw::EntryRow::builder().title("Modelo").text(&stt_config.model).build();
    let stt_menu = gio_menu_button(&STT_MODELS, glib::clone!(
        #[weak]
        stt_model,
        move |model: &str| stt_model.set_text(model)
    ));
    stt_model.add_suffix(&stt_menu);
    let stt_language = adw::EntryRow::builder()
        .title("Idioma (código como «es»; vacío = detectar)")
        .text(&stt_config.language)
        .build();
    let stt_key = adw::PasswordEntryRow::builder()
        .title("API key propia (opcional)")
        .text(secrets::get(STT_SECRET).unwrap_or_default())
        .show_apply_button(true)
        .build();
    let stt_url = adw::EntryRow::builder().title("URL base").text(&stt_config.base_url).build();
    for row in [
        stt_provider.upcast_ref::<gtk::Widget>(),
        stt_model.upcast_ref(),
        stt_language.upcast_ref(),
        stt_key.upcast_ref(),
        stt_url.upcast_ref(),
    ] {
        stt.add(row);
    }
    page.add(&stt);

    stt_provider.connect_selected_notify(glib::clone!(
        #[weak]
        prefs,
        #[weak]
        stt_url,
        move |row| {
            let provider = if row.selected() == 0 { SttProvider::Openai } else { SttProvider::OpenaiCompatible };
            prefs.config.borrow_mut().stt.provider = provider;
            if provider == SttProvider::Openai {
                stt_url.set_text("https://api.openai.com/v1");
            }
        }
    ));
    let bind = |row: &adw::EntryRow, apply: fn(&mut Config, String)| {
        row.connect_changed(glib::clone!(
            #[weak]
            prefs,
            move |row| apply(&mut prefs.config.borrow_mut(), row.text().trim().to_owned())
        ));
    };
    bind(&stt_model, |c, v| c.stt.model = v);
    bind(&stt_language, |c, v| c.stt.language = v);
    bind(&stt_url, |c, v| c.stt.base_url = v);
    stt_key.connect_apply(glib::clone!(
        #[weak]
        prefs,
        move |row| {
            let value = row.text().trim().to_owned();
            glib::spawn_future_local(async move { save_secret(&prefs, STT_SECRET.to_owned(), value).await });
        }
    ));
    page
}

fn recording_page(prefs: &Rc<Prefs>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder().name("recording").title("Grabación").icon_name("audio-input-microphone-symbolic").build();
    let recording = prefs.config.borrow().recording.clone();

    let group = adw::PreferencesGroup::builder().title("Grabación").build();
    let mode = adw::ComboRow::builder()
        .title("Modo del atajo")
        .model(&gtk::StringList::new(&RECORDING_MODES.map(|(_, label)| label)))
        .selected(RECORDING_MODES.iter().position(|(m, _)| *m == recording.mode).unwrap_or(0) as u32)
        .build();
    let silence = adw::SpinRow::builder()
        .title("Silencio para parar")
        .subtitle("Segundos sin voz tras los que se para en el modo automático")
        .adjustment(&gtk::Adjustment::new(recording.silence_seconds, 0.5, 10.0, 0.5, 1.0, 0.0))
        .digits(1)
        .build();
    let threshold = adw::SpinRow::builder()
        .title("Umbral de voz (dB)")
        .subtitle("Sube el número si el ruido de fondo impide que pare; bájalo si corta mientras hablas")
        .adjustment(&gtk::Adjustment::new(recording.voice_threshold_db, -70.0, -10.0, 1.0, 5.0, 0.0))
        .build();
    let max = adw::SpinRow::builder()
        .title("Duración máxima (segundos)")
        .adjustment(&gtk::Adjustment::new(f64::from(recording.max_seconds), 10.0, 1800.0, 10.0, 60.0, 0.0))
        .build();
    let auto_stop = recording.mode == RecordingMode::AutoStop;
    silence.set_sensitive(auto_stop);
    threshold.set_sensitive(auto_stop);

    let devices = audio::input_devices();
    let mut names = vec!["Predeterminado del sistema".to_owned()];
    names.extend(devices.iter().map(|(name, _)| name.clone()));
    let device = adw::ComboRow::builder()
        .title("Micrófono")
        .model(&gtk::StringList::new(&names.iter().map(String::as_str).collect::<Vec<_>>()))
        .selected(devices.iter().position(|(_, node)| *node == recording.device).map_or(0, |i| i + 1) as u32)
        .build();

    let feedback = adw::PreferencesGroup::builder().title("Avisos").build();
    let sounds = adw::SwitchRow::builder().title("Sonidos al empezar y al parar").active(recording.sounds).build();
    let indicator = adw::SwitchRow::builder()
        .title("Ventanita de grabación")
        .subtitle("Muestra el tiempo y el nivel del micrófono mientras grabas")
        .active(recording.indicator)
        .build();

    for row in [mode.upcast_ref::<gtk::Widget>(), device.upcast_ref(), max.upcast_ref(), silence.upcast_ref(), threshold.upcast_ref()] {
        group.add(row);
    }
    feedback.add(&sounds);
    feedback.add(&indicator);
    page.add(&group);
    page.add(&feedback);

    mode.connect_selected_notify(glib::clone!(
        #[weak]
        prefs,
        #[weak]
        silence,
        #[weak]
        threshold,
        move |row| {
            let mode = RECORDING_MODES[row.selected() as usize].0;
            prefs.config.borrow_mut().recording.mode = mode;
            silence.set_sensitive(mode == RecordingMode::AutoStop);
            threshold.set_sensitive(mode == RecordingMode::AutoStop);
        }
    ));
    device.connect_selected_notify(glib::clone!(
        #[weak]
        prefs,
        move |row| {
            let node = match row.selected() {
                0 => String::new(),
                i => devices.get(i as usize - 1).map(|(_, node)| node.clone()).unwrap_or_default(),
            };
            prefs.config.borrow_mut().recording.device = node;
        }
    ));
    silence.connect_value_notify(glib::clone!(
        #[weak]
        prefs,
        move |row| prefs.config.borrow_mut().recording.silence_seconds = row.value()
    ));
    threshold.connect_value_notify(glib::clone!(
        #[weak]
        prefs,
        move |row| prefs.config.borrow_mut().recording.voice_threshold_db = row.value()
    ));
    max.connect_value_notify(glib::clone!(
        #[weak]
        prefs,
        move |row| prefs.config.borrow_mut().recording.max_seconds = row.value() as u32
    ));
    sounds.connect_active_notify(glib::clone!(
        #[weak]
        prefs,
        move |row| prefs.config.borrow_mut().recording.sounds = row.is_active()
    ));
    indicator.connect_active_notify(glib::clone!(
        #[weak]
        prefs,
        move |row| prefs.config.borrow_mut().recording.indicator = row.is_active()
    ));
    page
}

fn data_page(prefs: &Rc<Prefs>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder().name("data").title("Datos").icon_name("drive-harddisk-symbolic").build();
    let audio_config = prefs.config.borrow().audio.clone();

    let group = adw::PreferencesGroup::builder()
        .title("Grabaciones")
        .description("Las grabaciones adjuntas a notas se conservan siempre.")
        .build();
    let retention = adw::ComboRow::builder()
        .title("Guardar el audio")
        .model(&gtk::StringList::new(&["Solo el adjunto a notas", "Siempre, en el historial"]))
        .selected(match audio_config.retention {
            AudioRetention::WithNotes => 0,
            AudioRetention::Always => 1,
        })
        .build();
    let days = adw::SpinRow::builder()
        .title("Borrar las del historial tras (días)")
        .subtitle("0 = no borrarlas nunca")
        .adjustment(&gtk::Adjustment::new(f64::from(audio_config.delete_after_days), 0.0, 3650.0, 1.0, 30.0, 0.0))
        .build();
    group.add(&retention);
    group.add(&days);
    page.add(&group);

    let folder = adw::PreferencesGroup::new();
    let open = adw::ActionRow::builder()
        .title("Carpeta de datos")
        .subtitle(config::data_dir().display().to_string())
        .activatable(true)
        .build();
    open.add_suffix(&gtk::Image::from_icon_name("folder-open-symbolic"));
    open.connect_activated(glib::clone!(
        #[weak(rename_to = dialog)]
        prefs.dialog,
        move |_| {
            let launcher = gtk::FileLauncher::new(Some(&gtk::gio::File::for_path(config::data_dir())));
            let window = dialog.root().and_downcast::<gtk::Window>();
            launcher.launch(window.as_ref(), gtk::gio::Cancellable::NONE, |result| {
                if let Err(err) = result {
                    log::warn!("No se pudo abrir la carpeta: {err}");
                }
            });
        }
    ));
    folder.add(&open);
    page.add(&folder);

    retention.connect_selected_notify(glib::clone!(
        #[weak]
        prefs,
        move |row| {
            prefs.config.borrow_mut().audio.retention =
                if row.selected() == 0 { AudioRetention::WithNotes } else { AudioRetention::Always };
        }
    ));
    days.connect_value_notify(glib::clone!(
        #[weak]
        prefs,
        move |row| prefs.config.borrow_mut().audio.delete_after_days = row.value() as u32
    ));
    page
}

/// Botón con una lista fija de opciones para rellenar una fila de texto.
fn gio_menu_button(options: &[&str], on_pick: impl Fn(&str) + Clone + 'static) -> gtk::MenuButton {
    let list = gtk::ListBox::builder().css_classes(["navigation-sidebar"]).build();
    for option in options {
        list.append(&gtk::Label::builder().label(*option).xalign(0.0).build());
    }
    let popover = gtk::Popover::builder().child(&list).build();
    list.connect_row_activated(glib::clone!(
        #[weak]
        popover,
        move |_, row| {
            if let Some(label) = row.child().and_downcast::<gtk::Label>() {
                on_pick(&label.text());
            }
            popover.popdown();
        }
    ));
    gtk::MenuButton::builder()
        .icon_name("pan-down-symbolic")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .popover(&popover)
        .build()
}
