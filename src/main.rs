mod app;
mod attachments;
mod attachment_view;
mod audio;
mod autostart;
mod camera;
mod config;
mod dev;
mod conversation_view;
mod explorer;
mod i18n;
mod indicator;
mod llm;
mod markdown;
mod popup;
mod preferences;
mod record_dialog;
mod reminder_window;
mod reminders;
mod runtime;
mod secrets;
mod shortcuts;
mod store;
mod stt;
mod tools;
mod tray;

use std::cell::OnceCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use app::{Command, Controller};
use i18n::{tr, trf};

fn usage() -> String {
    let commands = [
        ("toggle", tr("Empezar o parar una grabación")),
        ("new", tr("Abrir el popup para escribir")),
        ("reopen", tr("Mostrar la última conversación")),
        ("explorer", tr("Abrir la ventana de notas, tareas e historial")),
        ("preferences", tr("Abrir las preferencias")),
        ("open ID", tr("Abrir una conversación del historial para seguirla")),
        ("quit", tr("Cerrar el asistente")),
        ("history", tr("Listar las conversaciones guardadas")),
        ("models", tr("Listar los modelos del proveedor de LLM configurado")),
        ("config", tr("Mostrar la ruta del archivo de configuración")),
    ];
    let mut text = format!(
        "{}\n\n{}\n{}\n\n",
        tr("Uso: gnome-personal-assistant [ORDEN]"),
        tr("Sin orden arranca el asistente en segundo plano (icono de bandeja y atajos)."),
        tr("Si ya está en marcha, las órdenes se envían a esa instancia:"),
    );
    for (command, description) in commands {
        text.push_str(&format!("  {command:<12} {description}\n"));
    }
    text
}

fn main() -> glib::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    i18n::init(&config::Config::load().map(|c| c.language).unwrap_or_default());

    // Órdenes que no necesitan la instancia gráfica.
    match std::env::args().nth(1).as_deref() {
        Some("-h" | "--help" | "help") => {
            println!("{}", usage());
            return glib::ExitCode::SUCCESS;
        }
        Some("models") => return list_models(),
        Some("history") => return list_history(),
        Some("config") => {
            if let Err(err) = config::Config::load() {
                eprintln!("{err:#}");
            }
            println!("{}", config::config_path().display());
            return glib::ExitCode::SUCCESS;
        }
        _ => {}
    }

    if let Err(err) = gst::init() {
        eprintln!("{}", trf("No se pudo iniciar GStreamer: {error}", &[("error", &err)]));
        return glib::ExitCode::FAILURE;
    }

    let mut flags = gio::ApplicationFlags::HANDLES_COMMAND_LINE;
    if dev::enabled() {
        flags |= gio::ApplicationFlags::NON_UNIQUE;
    }
    let app = adw::Application::builder().application_id(config::APP_ID).flags(flags).build();

    let (sender, receiver) = async_channel::unbounded::<Command>();
    let controller: Rc<OnceCell<Rc<Controller>>> = Rc::default();

    app.connect_startup({
        let sender = sender.clone();
        let controller = controller.clone();
        move |app| {
            load_css();
            add_actions(app, &sender);
            // La app sigue viva sin ventanas abiertas.
            std::mem::forget(app.hold());
            let _ = controller.set(Controller::start(app, sender.clone(), receiver.clone()));
        }
    });

    app.connect_command_line(move |_app, cmdline| {
        let args = cmdline.arguments();
        let command = args.get(1).map(|a| a.to_string_lossy().into_owned());
        let command = match command.as_deref() {
            None if cmdline.is_remote() => Some(Command::ShowExplorer),
            None => None,
            Some("toggle") => Some(Command::ToggleRecording),
            Some("new") => Some(Command::NewTextConversation),
            Some("reopen") => Some(Command::Reopen),
            Some("explorer") => Some(Command::ShowExplorer),
            Some("preferences") => Some(Command::ShowPreferences),
            Some("quit") => Some(Command::Quit),
            Some("open") => match args.get(2).and_then(|id| id.to_str()?.parse().ok()) {
                Some(id) => Some(Command::OpenConversation(id)),
                None => {
                    cmdline.printerr_literal(&format!("{}\n", tr("Uso: gnome-personal-assistant open ID")));
                    return glib::ExitCode::FAILURE;
                }
            },
            Some(other) => {
                cmdline.printerr_literal(&format!("{}\n\n{}", trf("Orden desconocida: {command}", &[("command", &other)]), usage()));
                return glib::ExitCode::FAILURE;
            }
        };
        if let Some(command) = command {
            let _ = sender.try_send(command);
        }
        glib::ExitCode::SUCCESS
    });

    app.run()
}

fn add_actions(app: &adw::Application, sender: &async_channel::Sender<Command>) {
    let actions = [
        ("toggle", Command::ToggleRecording),
        ("new", Command::NewTextConversation),
        ("reopen", Command::Reopen),
        ("explorer", Command::ShowExplorer),
        ("preferences", Command::ShowPreferences),
        ("quit", Command::Quit),
    ];
    for (name, command) in actions {
        let action = gio::SimpleAction::new(name, None);
        let sender = sender.clone();
        action.connect_activate(move |_, _| {
            let _ = sender.try_send(command.clone());
        });
        app.add_action(&action);
    }
    // Botones de los recordatorios: llevan el id de la tarea.
    let reminder_snooze = |id| Command::ReminderSnooze(id, None);
    for (name, command) in [
        ("todo-done", Command::TodoDone as fn(i64) -> Command),
        ("todo-snooze", Command::TodoSnooze),
        ("reminder-done", Command::ReminderDone),
        ("reminder-snooze", reminder_snooze),
        ("reminder-show", Command::ShowReminder),
    ] {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::INT64));
        let sender = sender.clone();
        action.connect_activate(move |_, parameter| {
            if let Some(id) = parameter.and_then(|p| p.get::<i64>()) {
                let _ = sender.try_send(command(id));
            }
        });
        app.add_action(&action);
    }
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(conversation_view::CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn list_models() -> glib::ExitCode {
    let result = config::Config::load().and_then(|config| {
        if let Err(err) = runtime::runtime().block_on(secrets::load()) {
            eprintln!("Llavero: {err:#}");
        }
        let client = llm::Client::from_config(&config.llm)?;
        println!("{}", trf("{provider} (modelo actual: {model})", &[("provider", &client.provider.label()), ("model", &client.model)]));
        runtime::runtime().block_on(client.list_models())
    });
    match result {
        Ok(models) => {
            for model in models {
                println!("  {model}");
            }
            glib::ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("{err:#}");
            glib::ExitCode::FAILURE
        }
    }
}

fn list_history() -> glib::ExitCode {
    let result = store::Store::open(&config::data_dir().join("assistant.db"))
        .and_then(|store| store.list_conversations(50));
    match result {
        Ok(conversations) => {
            for c in conversations {
                let when = chrono::DateTime::from_timestamp(c.modified, 0)
                    .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default();
                println!("{:>5}  {when}  {:<10} {}", c.id, c.provider, c.title);
            }
            glib::ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("{err:#}");
            glib::ExitCode::FAILURE
        }
    }
}
