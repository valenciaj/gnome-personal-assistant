//! API keys en el llavero de GNOME (Secret Service), con una caché en memoria
//! para poder leerlas sin esperar a D-Bus.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use anyhow::{Context, Result};

use crate::config::{self, APP_ID, Config, LlmProvider};
use crate::i18n::tr;

const NAME_ATTRIBUTE: &str = "key-name";

fn cache() -> &'static RwLock<HashMap<String, String>> {
    static CACHE: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

pub fn get(name: &str) -> Option<String> {
    cache().read().ok()?.get(name).filter(|v| !v.is_empty()).cloned()
}

/// Carga en la caché todas las claves de la app.
pub async fn load() -> Result<()> {
    let keyring = oo7::Keyring::new().await.with_context(|| tr("No se pudo abrir el llavero"))?;
    keyring.unlock().await?;
    let items = keyring.search_items(&[("application", APP_ID)]).await?;
    let mut loaded = HashMap::new();
    for item in items {
        let attributes = item.attributes().await?;
        if let Some(name) = attributes.get(NAME_ATTRIBUTE) {
            let secret = item.secret().await?;
            loaded.insert(name.clone(), String::from_utf8_lossy(secret.as_bytes()).into_owned());
        }
    }
    log::info!("{} claves cargadas del llavero", loaded.len());
    if let Ok(mut cache) = cache().write() {
        cache.extend(loaded);
    }
    Ok(())
}

/// Guarda (o borra, si `value` está vacío) una clave en el llavero.
pub async fn set(name: &str, value: &str) -> Result<()> {
    let keyring = oo7::Keyring::new().await.with_context(|| tr("No se pudo abrir el llavero"))?;
    keyring.unlock().await?;
    let attributes = [("application", APP_ID), (NAME_ATTRIBUTE, name)];
    if value.is_empty() {
        keyring.delete(&attributes).await?;
    } else {
        let label = format!("Asistente personal: {name}");
        keyring.create_item(&label, &attributes, value, true).await?;
    }
    if let Ok(mut cache) = cache().write() {
        cache.insert(name.to_owned(), value.to_owned());
    }
    Ok(())
}

/// Mueve al llavero las claves que estaban en texto plano en config.toml.
pub async fn migrate_from_config() -> Result<()> {
    let mut config = Config::load()?;
    let mut moved = false;
    for provider in LlmProvider::ALL {
        let key = std::mem::take(&mut config.llm.settings_mut(provider).api_key);
        if !key.is_empty() {
            set(&provider.secret_name(), &key).await?;
            moved = true;
        }
    }
    let key = std::mem::take(&mut config.stt.api_key);
    if !key.is_empty() {
        set(config::STT_SECRET, &key).await?;
        moved = true;
    }
    if moved {
        config.save()?;
        log::info!("API keys movidas de config.toml al llavero");
    }
    Ok(())
}
