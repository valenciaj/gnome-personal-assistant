//! Atajos globales mediante el portal XDG GlobalShortcuts.
//!
//! En Wayland una app no puede capturar teclas fuera de sus ventanas: se las pide al
//! portal, que muestra a la persona un diálogo para confirmar o cambiar la combinación.

use anyhow::{Context, Result};
use ashpd::desktop::global_shortcuts::{
    BindShortcutsOptions, ConfigureShortcutsOptions, GlobalShortcuts, ListShortcutsOptions,
    NewShortcut,
};
use ashpd::desktop::{CreateSessionOptions, Session};
use futures_util::StreamExt;

use crate::app::Command;
use crate::config::APP_ID;
use crate::i18n::tr;

pub const RECORD: &str = "record";
pub const REOPEN: &str = "reopen";
pub const CAPTURE: &str = "capture";

/// Sesión viva del portal; se necesita para abrir el diálogo de configuración.
pub struct Shortcuts {
    portal: GlobalShortcuts,
    session: Session<GlobalShortcuts>,
}

impl Shortcuts {
    /// Registra los atajos y reenvía sus pulsaciones al hilo de GTK por `commands`.
    pub async fn register(commands: async_channel::Sender<Command>) -> Result<Self> {
        // Una app sin sandbox tiene que identificarse ante el portal antes de usarlo.
        ashpd::register_host_app(APP_ID.try_into()?)
            .await
            .with_context(|| tr("No se pudo registrar la app en el portal"))?;

        let portal = GlobalShortcuts::new()
            .await
            .with_context(|| tr("El portal GlobalShortcuts no está disponible"))?;
        let session = portal.create_session(CreateSessionOptions::default()).await?;

        // GNOME recuerda los atajos de cada app; solo se piden (con diálogo) la primera vez.
        let existing = portal
            .list_shortcuts(&session, ListShortcutsOptions::default())
            .await?
            .response()?;
        let known = |id: &str| existing.shortcuts().iter().any(|s| s.id() == id);
        let bound = if known(RECORD) && known(REOPEN) && known(CAPTURE) {
            existing
                .shortcuts()
                .iter()
                .map(|s| (s.id().to_owned(), s.trigger_description().to_owned()))
                .collect::<Vec<_>>()
        } else {
            let shortcuts = [
                NewShortcut::new(RECORD, tr("Grabar y preguntar al asistente"))
                    .preferred_trigger("LOGO+ALT+space"),
                NewShortcut::new(REOPEN, tr("Reabrir la última conversación"))
                    .preferred_trigger("LOGO+ALT+r"),
                NewShortcut::new(CAPTURE, tr("Capturar la pantalla y preguntar"))
                    .preferred_trigger("LOGO+ALT+s"),
            ];
            portal
                .bind_shortcuts(&session, &shortcuts, None, BindShortcutsOptions::default())
                .await?
                .response()?
                .shortcuts()
                .iter()
                .map(|s| (s.id().to_owned(), s.trigger_description().to_owned()))
                .collect()
        };
        for (id, trigger) in &bound {
            log::info!("Atajo {id} → {trigger}");
        }

        let mut activated = portal.receive_activated().await?;
        let mut deactivated = portal.receive_deactivated().await?;
        let tx = commands.clone();
        tokio::spawn(async move {
            while let Some(event) = activated.next().await {
                let token = event
                    .options()
                    .get("activation_token")
                    .and_then(|v| String::try_from(v.try_clone().ok()?).ok());
                let command = match event.shortcut_id() {
                    RECORD => Command::ShortcutPressed { token },
                    REOPEN => Command::Reopen,
                    CAPTURE => Command::CaptureAndAsk { token },
                    _ => continue,
                };
                if tx.send(command).await.is_err() {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            while let Some(event) = deactivated.next().await {
                if event.shortcut_id() == RECORD && commands.send(Command::ShortcutReleased).await.is_err() {
                    break;
                }
            }
        });

        Ok(Self { portal, session })
    }

    /// Abre el diálogo del sistema para cambiar las combinaciones.
    pub async fn configure(&self) -> Result<()> {
        self.portal
            .configure_shortcuts(&self.session, None, ConfigureShortcutsOptions::default())
            .await?;
        Ok(())
    }
}
