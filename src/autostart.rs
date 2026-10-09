//! Arranque al iniciar sesión: un .desktop en ~/.config/autostart.

use std::path::PathBuf;

use anyhow::Result;

use crate::config::APP_ID;

fn path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("autostart").join(format!("{APP_ID}.desktop"))
}

pub fn is_enabled() -> bool {
    path().exists()
}

pub fn set_enabled(enabled: bool) -> Result<()> {
    let path = path();
    if !enabled {
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        return Ok(());
    }
    // El binario que está en marcha: /usr/bin si está instalado, target/ en desarrollo.
    let exe = std::env::current_exe()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &path,
        format!(
            "[Desktop Entry]\nType=Application\nName=Asistente personal\nExec={}\nIcon={APP_ID}\n\
             X-GNOME-Autostart-enabled=true\nNoDisplay=true\n",
            exe.display()
        ),
    )?;
    Ok(())
}
