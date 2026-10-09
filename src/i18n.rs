//! Traducciones. Las cadenas del código están en español; `po/en.po` las traduce
//! al inglés, que se usa en cualquier idioma del sistema distinto del español.
//!
//! Extracción: `scripts/update-po.sh` (xgettext con las palabras clave tr, trf y n_).

use std::fmt::Display;
use std::sync::OnceLock;

static CATALOG: OnceLock<Option<gettext::Catalog>> = OnceLock::new();
const EN_MO: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/en.mo"));

/// `preference`: "es", "en" o "auto" (idioma del sistema).
pub fn init(preference: &str) {
    let language = match preference {
        "es" | "en" => preference,
        _ => system_language(),
    };
    let catalog = (language == "en").then(|| gettext::Catalog::parse(EN_MO).ok()).flatten();
    let _ = CATALOG.set(catalog);
}

fn system_language() -> &'static str {
    let locale = ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|var| std::env::var(var).ok())
        .map(|value| value.split(':').next().unwrap_or_default().to_owned())
        .find(|value| !value.is_empty())
        .unwrap_or_default();
    if locale.starts_with("es") { "es" } else { "en" }
}

pub fn tr(msgid: &str) -> String {
    match CATALOG.get() {
        Some(Some(catalog)) => catalog.gettext(msgid).to_owned(),
        _ => msgid.to_owned(),
    }
}

/// Traduce y sustituye los `{nombre}` del texto.
pub fn trf(msgid: &str, args: &[(&str, &(dyn Display + Sync))]) -> String {
    let mut text = tr(msgid);
    for (name, value) in args {
        text = text.replace(&format!("{{{name}}}"), &value.to_string());
    }
    text
}

/// Marca una cadena para extraerla sin traducirla aún (se traduce al mostrarla).
pub const fn n_(msgid: &'static str) -> &'static str {
    msgid
}
