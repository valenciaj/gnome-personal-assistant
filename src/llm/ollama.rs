//! API nativa de Ollama (/api/chat), que admite tools y `think`.

use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::{Client, ToolCall, ToolResult, ToolSpec, Turn, json_response, openai};
use crate::i18n::trf;

pub async fn complete(
    client: &Client,
    system: &str,
    messages: &[Value],
    tools: &[ToolSpec],
) -> Result<Turn> {
    let mut all = Vec::with_capacity(messages.len() + 1);
    all.push(json!({ "role": "system", "content": system }));
    all.extend_from_slice(messages);

    let mut body = json!({ "model": client.model, "messages": all, "stream": false });
    if !tools.is_empty() {
        // Mismo formato de tools que OpenAI.
        body["tools"] = openai::tools_json(tools);
    }
    match client.reasoning_level() {
        None => {}
        Some("off") => body["think"] = json!(false),
        Some("on") => body["think"] = json!(true),
        // Algunos modelos (gpt-oss) aceptan un nivel en lugar de un booleano.
        Some(level) => body["think"] = json!(level),
    }

    let response = client
        .http
        .post(format!("{}/api/chat", client.base_url))
        .json(&body)
        .send()
        .await?;
    let response = json_response(response).await?;
    let Some(message) = response.get("message") else {
        bail!(trf("Respuesta sin mensaje: {response}", &[("response", &response)]));
    };

    let text = message["content"].as_str().unwrap_or_default().to_owned();
    let reasoning = message["thinking"].as_str().unwrap_or_default().to_owned();
    let tool_calls = message["tool_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, c)| ToolCall {
            // Ollama no da ids; el resultado se asocia por nombre.
            id: format!("call_{i}"),
            name: c.pointer("/function/name").and_then(Value::as_str).unwrap_or_default().to_owned(),
            input: c.pointer("/function/arguments").cloned().unwrap_or_else(|| json!({})),
        })
        .collect();

    Ok(Turn { native: vec![message.clone()], text, reasoning, tool_calls })
}

pub fn tool_result_messages(results: &[ToolResult]) -> Vec<Value> {
    results
        .iter()
        .map(|r| json!({ "role": "tool", "tool_name": r.call.name, "content": r.content }))
        .collect()
}

pub async fn list_models(client: &Client) -> Result<Vec<String>> {
    let response = client.http.get(format!("{}/api/tags", client.base_url)).send().await?;
    let body = json_response(response).await?;
    Ok(body["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["name"].as_str().map(str::to_owned))
        .collect())
}
