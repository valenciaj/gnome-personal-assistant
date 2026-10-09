//! OpenAI Chat Completions; sirve también para cualquier servidor compatible
//! (OpenRouter, Groq, Mistral, LM Studio, vLLM…).

use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::{Client, ToolCall, ToolResult, ToolSpec, Turn, json_response};

pub async fn complete(
    client: &Client,
    system: &str,
    messages: &[Value],
    tools: &[ToolSpec],
) -> Result<Turn> {
    let mut all = Vec::with_capacity(messages.len() + 1);
    all.push(json!({ "role": "system", "content": system }));
    all.extend_from_slice(messages);

    let mut body = json!({ "model": client.model, "messages": all });
    if !tools.is_empty() {
        body["tools"] = tools_json(tools);
    }
    if let Some(effort) = client.reasoning_level() {
        body["reasoning_effort"] = json!(effort);
    }

    let mut request = client
        .http
        .post(format!("{}/chat/completions", client.base_url))
        .json(&body);
    if !client.api_key.is_empty() {
        request = request.bearer_auth(&client.api_key);
    }
    let response = json_response(request.send().await?).await?;

    let Some(message) = response.pointer("/choices/0/message") else {
        bail!("Respuesta sin mensaje: {response}");
    };
    if let Some(refusal) = message["refusal"].as_str() {
        bail!("El modelo ha rechazado la petición: {refusal}");
    }

    let text = message["content"].as_str().unwrap_or_default().to_owned();
    // No es estándar: DeepSeek/vLLM usan `reasoning_content`; OpenRouter/Groq, `reasoning`.
    let reasoning = message["reasoning_content"]
        .as_str()
        .or_else(|| message["reasoning"].as_str())
        .unwrap_or_default()
        .to_owned();

    let raw_calls = message["tool_calls"].as_array().cloned().unwrap_or_default();
    let tool_calls = raw_calls
        .iter()
        .map(|c| ToolCall {
            id: c["id"].as_str().unwrap_or_default().to_owned(),
            name: c.pointer("/function/name").and_then(Value::as_str).unwrap_or_default().to_owned(),
            input: c
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .and_then(|args| serde_json::from_str(args).ok())
                .unwrap_or_else(|| json!({})),
        })
        .collect();

    let mut native = json!({ "role": "assistant", "content": text });
    if !raw_calls.is_empty() {
        native["tool_calls"] = Value::Array(raw_calls);
    }
    Ok(Turn { native, text, reasoning, tool_calls })
}

pub fn tools_json(tools: &[ToolSpec]) -> Value {
    tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": { "name": t.name, "description": t.description, "parameters": t.schema },
            })
        })
        .collect()
}

pub fn tool_result_messages(results: &[ToolResult]) -> Vec<Value> {
    results
        .iter()
        .map(|r| json!({ "role": "tool", "tool_call_id": r.call.id, "content": r.content }))
        .collect()
}

pub async fn list_models(client: &Client) -> Result<Vec<String>> {
    let mut request = client.http.get(format!("{}/models", client.base_url));
    if !client.api_key.is_empty() {
        request = request.bearer_auth(&client.api_key);
    }
    let body = json_response(request.send().await?).await?;
    Ok(body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["id"].as_str().map(str::to_owned))
        .collect())
}
