mod app;
mod audio;
mod config;
mod llm;
mod markdown;
mod popup;
mod runtime;
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

const USAGE: &str = "\
Uso: gnome-personal-assistant [ORDEN]

Sin orden arranca el asistente en segundo plano (icono de bandeja + atajos).
Si ya está en marcha, las órdenes se envían a esa instancia:

  toggle    Empezar o parar una grabación
  new       Abrir el popup para escribir
  reopen    Mostrar la última conversación
  open ID   Abrir una conversación del historial para seguirla
  quit      Cerrar el asistente
  history   Listar las conversaciones guardadas
  models    Listar los modelos del proveedor de LLM configurado
  config    Mostrar la ruta del archivo de configuración";

fn main() -> glib::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Órdenes que no necesitan la instancia gráfica.
    match std::env::args().nth(1).as_deref() {
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
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
        eprintln!("No se pudo iniciar GStreamer: {err}");
        return glib::ExitCode::FAILURE;
    }

    let app = adw::Application::builder()
        .application_id(config::APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

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
            None if cmdline.is_remote() => Some(Command::Reopen),
            None => None,
            Some("toggle") => Some(Command::ToggleRecording),
            Some("new") => Some(Command::NewTextConversation),
            Some("reopen") => Some(Command::Reopen),
            Some("quit") => Some(Command::Quit),
            Some("open") => match args.get(2).and_then(|id| id.to_str()?.parse().ok()) {
                Some(id) => Some(Command::OpenConversation(id)),
                None => {
                    cmdline.printerr_literal("Uso: gnome-personal-assistant open ID\n");
                    return glib::ExitCode::FAILURE;
                }
            },
            Some(other) => {
                cmdline.printerr_literal(&format!("Orden desconocida: {other}\n\n{USAGE}\n"));
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
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(popup::CSS);
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
        let client = llm::Client::from_config(&config.llm)?;
        println!("{} (modelo actual: {})", client.provider.label(), client.model);
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
