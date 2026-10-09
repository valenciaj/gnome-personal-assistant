//! Anthropic Messages API (HTTP directo: no hay SDK oficial para Rust).

use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::{Client, ToolCall, ToolResult, ToolSpec, Turn, json_response};
use crate::i18n::{tr, trf};

const API_VERSION: &str = "2023-06-01";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

/// Modelos que aceptan `fallbacks: "default"`: si el modelo rechaza una petición por
/// sus filtros de seguridad, la API la reintenta en otro modelo en la misma llamada.
fn supports_server_fallback(model: &str) -> bool {
    ["claude-opus-5-5", "claude-opus-5", "claude-fable-5-1", "claude-fable-5", "claude-sonnet-5-5"]
        .contains(&model)
}

pub async fn complete(
    client: &Client,
    system: &str,
    messages: &[Value],
    tools: &[ToolSpec],
) -> Result<Turn> {
    let mut body = json!({
        "model": client.model,
        "max_tokens": 16000,
        "system": system,
        "messages": messages,
    });
    if !tools.is_empty() {
        body["tools"] = tools
            .iter()
            .map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.schema }))
            .collect();
    }
    if let Some(effort) = client.reasoning_level() {
        body["thinking"] = json!({ "type": "adaptive", "display": "summarized" });
        body["output_config"] = json!({ "effort": effort });
    }

    let mut request = client
        .http
        .post(format!("{}/v1/messages", client.base_url))
        .header("x-api-key", &client.api_key)
        .header("anthropic-version", API_VERSION);
    if supports_server_fallback(&client.model) {
        body["fallbacks"] = json!("default");
        request = request.header("anthropic-beta", FALLBACK_BETA);
    }

    let response = json_response(request.json(&body).send().await?).await?;
    parse_response(response)
}

fn parse_response(response: Value) -> Result<Turn> {
    let content = response["content"].as_array().cloned().unwrap_or_default();
    match response["stop_reason"].as_str() {
        Some("refusal") => {
            let category = response
                .pointer("/stop_details/category")
                .and_then(Value::as_str)
                .map_or_else(|| tr("sin categoría"), str::to_owned);
            bail!(trf("El modelo ha rechazado la petición ({category})", &[("category", &category)]));
        }
        Some("max_tokens") => log::warn!("Respuesta truncada por max_tokens"),
        _ => {}
    }

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();
    for block in &content {
        match block["type"].as_str() {
            Some("text") => text.push_str(block["text"].as_str().unwrap_or_default()),
            Some("thinking") => {
                let thought = block["thinking"].as_str().unwrap_or_default();
                if !thought.is_empty() {
                    if !reasoning.is_empty() {
                        reasoning.push_str("\n\n");
                    }
                    reasoning.push_str(thought);
                }
            }
            Some("tool_use") => tool_calls.push(ToolCall {
                id: block["id"].as_str().unwrap_or_default().to_owned(),
                name: block["name"].as_str().unwrap_or_default().to_owned(),
                input: block["input"].clone(),
            }),
            _ => {}
        }
    }

    // El contenido se guarda íntegro (thinking, fallback…) para reenviarlo sin cambios.
    Ok(Turn {
        native: vec![json!({ "role": "assistant", "content": content })],
        text,
        reasoning,
        tool_calls,
    })
}

/// Todos los resultados van en un único mensaje de usuario.
pub fn tool_result_messages(results: &[ToolResult]) -> Vec<Value> {
    let blocks: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "type": "tool_result",
                "tool_use_id": r.call.id,
                "content": r.content,
                "is_error": r.is_error,
            })
        })
        .collect();
    vec![json!({ "role": "user", "content": blocks })]
}

pub async fn list_models(client: &Client) -> Result<Vec<String>> {
    let response = client
        .http
        .get(format!("{}/v1/models?limit=1000", client.base_url))
        .header("x-api-key", &client.api_key)
        .header("anthropic-version", API_VERSION)
        .send()
        .await?;
    let body = json_response(response).await?;
    Ok(body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["id"].as_str().map(str::to_owned))
        .collect())
}
