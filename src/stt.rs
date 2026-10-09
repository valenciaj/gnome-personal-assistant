//! Transcripción con la API /audio/transcriptions de OpenAI (o compatible).

use std::path::Path;

use anyhow::{Context, Result, bail};
use reqwest::multipart::{Form, Part};

use crate::config::SttConfig;
use crate::llm::http_client;
use crate::i18n::{tr, trf};

pub async fn transcribe(config: &SttConfig, audio: &Path) -> Result<String> {
    let api_key = config.api_key();
    if api_key.is_empty() && config.base_url.contains("api.openai.com") {
        bail!(tr("Falta la API key de OpenAI para transcribir"));
    }
    let bytes = tokio::fs::read(audio)
        .await
        .with_context(|| trf("No se pudo leer {path}", &[("path", &audio.display())]))?;

    let file = Part::bytes(bytes).file_name("audio.ogg").mime_str("audio/ogg")?;
    let mut form = Form::new()
        .part("file", file)
        .text("model", config.model.clone())
        .text("response_format", "json");
    if !config.language.trim().is_empty() {
        form = form.text("language", config.language.trim().to_owned());
    }

    let url = format!("{}/audio/transcriptions", config.base_url.trim_end_matches('/'));
    let mut request = http_client()?.post(url).multipart(form);
    if !api_key.is_empty() {
        request = request.bearer_auth(api_key);
    }
    let response = crate::llm::json_response(request.send().await?).await?;
    Ok(response["text"].as_str().unwrap_or_default().trim().to_owned())
}
