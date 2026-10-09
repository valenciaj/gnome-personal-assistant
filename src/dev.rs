//! Modo de desarrollo (variables de entorno, no afectan al uso normal).
//!
//! - `ASSISTANT_DEV=1`: instancia independiente (no única), sin bandeja ni atajos,
//!   para probar una compilación nueva mientras otra está en marcha.
//! - `ASSISTANT_SNAPSHOT=dir`: abre el explorador, guarda una captura PNG de cada
//!   pestaña en `dir` y termina.

use std::path::{Path, PathBuf};
use std::time::Duration;

use adw::prelude::*;
use gtk::{glib, graphene};

pub fn enabled() -> bool {
    std::env::var_os("ASSISTANT_DEV").is_some() || snapshot_dir().is_some()
}

pub fn snapshot_dir() -> Option<PathBuf> {
    std::env::var_os("ASSISTANT_SNAPSHOT").map(PathBuf::from)
}

/// Captura el contenido de una ventana en un PNG.
pub fn save_png(window: &impl IsA<gtk::Widget>, path: &Path) -> anyhow::Result<()> {
    let widget = window.as_ref();
    let (width, height) = (widget.width(), widget.height());
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
    let node = snapshot.to_node().ok_or_else(|| anyhow::anyhow!("ventana vacía ({width}×{height}, visible: {})", widget.is_visible()))?;
    let renderer = widget
        .native()
        .and_then(|n| n.renderer())
        .ok_or_else(|| anyhow::anyhow!("la ventana no tiene renderer"))?;
    let bounds = graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
    renderer.render_texture(&node, Some(&bounds)).save_to_png(path)?;
    Ok(())
}

/// Ejecuta `steps` en orden, dejando `delay` entre uno y otro para que GTK dibuje.
pub fn run_steps(delay: Duration, steps: Vec<Box<dyn FnOnce()>>) {
    fn schedule(delay: Duration, mut steps: std::vec::IntoIter<Box<dyn FnOnce()>>) {
        glib::timeout_add_local_once(delay, move || {
            if let Some(step) = steps.next() {
                step();
                schedule(delay, steps);
            }
        });
    }
    schedule(delay, steps.into_iter());
}
