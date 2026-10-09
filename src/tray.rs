//! Icono de bandeja (StatusNotifierItem). En Ubuntu lo muestra la extensión AppIndicator.

use ksni::TrayMethods;

use crate::app::Command;
use crate::i18n::{n_, tr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayState {
    Idle,
    Recording,
    Busy,
    Error,
}

pub struct AppTray {
    state: TrayState,
    commands: async_channel::Sender<Command>,
}

impl AppTray {
    fn send(&self, command: Command) {
        let _ = self.commands.try_send(command);
    }
}

impl ksni::Tray for AppTray {
    fn id(&self) -> String {
        crate::config::APP_ID.into()
    }

    fn title(&self) -> String {
        tr("Asistente personal")
    }

    fn icon_name(&self) -> String {
        match self.state {
            TrayState::Idle => "audio-input-microphone-symbolic",
            TrayState::Recording => "media-record-symbolic",
            TrayState::Busy => "process-working-symbolic",
            TrayState::Error => "dialog-warning-symbolic",
        }
        .into()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let description = match self.state {
            TrayState::Idle => tr("Listo"),
            TrayState::Recording => tr("Grabando…"),
            TrayState::Busy => tr("Pensando…"),
            TrayState::Error => tr("Ha ocurrido un error"),
        };
        ksni::ToolTip {
            title: tr("Asistente personal"),
            description,
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(Command::Reopen);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        let item = |label: &str, icon: &str, command: Command| -> ksni::MenuItem<Self> {
            StandardItem {
                label: tr(label),
                icon_name: icon.into(),
                activate: Box::new(move |this: &mut Self| this.send(command.clone())),
                ..Default::default()
            }
            .into()
        };
        let record = if self.state == TrayState::Recording {
            item(n_("Parar grabación"), "media-playback-stop-symbolic", Command::ToggleRecording)
        } else {
            item(n_("Grabar"), "audio-input-microphone-symbolic", Command::ToggleRecording)
        };
        vec![
            record,
            item(n_("Escribir…"), "document-edit-symbolic", Command::NewTextConversation),
            item(n_("Última conversación"), "user-available-symbolic", Command::Reopen),
            item(n_("Notas, tareas e historial…"), "view-list-bullet-symbolic", Command::ShowExplorer),
            ksni::MenuItem::Separator,
            item(n_("Preferencias…"), "preferences-system-symbolic", Command::ShowPreferences),
            item(n_("Salir"), "application-exit-symbolic", Command::Quit),
        ]
    }
}

pub type TrayHandle = ksni::Handle<AppTray>;

pub async fn spawn(commands: async_channel::Sender<Command>) -> anyhow::Result<TrayHandle> {
    let tray = AppTray { state: TrayState::Idle, commands };
    Ok(tray.spawn().await?)
}

pub async fn set_state(handle: &TrayHandle, state: TrayState) {
    handle.update(|tray| tray.state = state).await;
}
