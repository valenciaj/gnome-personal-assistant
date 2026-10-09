//! API Responses de OpenAI (/v1/responses). Es la única de OpenAI que permite usar
//! tools con razonamiento en los modelos actuales.
//!
//! Se usa sin estado (`store: false`): el historial completo viaja en cada petición
//! y el razonamiento vuelve cifrado (`reasoning.encrypted_content`) para que el
//! modelo lo recupere en el siguiente turno.

use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::{Client, ToolCall, ToolResult, ToolSpec, Turn, json_response};
use crate::i18n::trf;

pub async fn complete(
    client: &Client,
    system: &str,
    messages: &[Value],
    tools: &[ToolSpec],
) -> Result<Turn> {
    // Sin el contenido cifrado (modelo sin razonamiento o sin pedirlo) no se pueden
    // reenviar los elementos de razonamiento, porque no se guardan en OpenAI.
    let input: Vec<&Value> = messages
        .iter()
        .filter(|item| item["type"] != "reasoning" || item.get("encrypted_content").is_some_and(|c| !c.is_null()))
        .collect();

    let mut body = json!({
        "model": client.model,
        "instructions": system,
        "input": input,
        "store": false,
    });
    if !tools.is_empty() {
        body["tools"] = tools
            .iter()
            .map(|t| json!({ "type": "function", "name": t.name, "description": t.description, "parameters": t.schema }))
            .collect();
    }
    // Solo se pide razonamiento si está configurado: los modelos que no razonan
    // rechazan el parámetro.
    if let Some(effort) = client.reasoning_level() {
        body["reasoning"] = json!({ "effort": effort, "summary": "auto" });
        body["include"] = json!(["reasoning.encrypted_content"]);
    }

    let response = client
        .http
        .post(format!("{}/responses", client.base_url))
        .bearer_auth(&client.api_key)
        .json(&body)
        .send()
        .await?;
    parse_response(json_response(response).await?)
}

fn parse_response(response: Value) -> Result<Turn> {
    if response["status"] == "incomplete" {
        let reason = response.pointer("/incomplete_details/reason").and_then(Value::as_str).unwrap_or("?");
        log::warn!("Respuesta incompleta: {reason}");
    }
    let output = response["output"].as_array().cloned().unwrap_or_default();

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();
    for item in &output {
        match item["type"].as_str() {
            Some("message") => {
                for part in item["content"].as_array().into_iter().flatten() {
                    match part["type"].as_str() {
                        Some("output_text") => text.push_str(part["text"].as_str().unwrap_or_default()),
                        Some("refusal") => {
                            let reason = part["refusal"].as_str().unwrap_or_default();
                            bail!(trf("El modelo ha rechazado la petición: {reason}", &[("reason", &reason)]));
                        }
                        _ => {}
                    }
                }
            }
            Some("reasoning") => {
                for summary in item["summary"].as_array().into_iter().flatten() {
                    if let Some(part) = summary["text"].as_str().filter(|t| !t.is_empty()) {
                        if !reasoning.is_empty() {
                            reasoning.push_str("\n\n");
                        }
                        reasoning.push_str(part);
                    }
                }
            }
            Some("function_call") => tool_calls.push(ToolCall {
                id: item["call_id"].as_str().unwrap_or_default().to_owned(),
                name: item["name"].as_str().unwrap_or_default().to_owned(),
                input: item["arguments"]
                    .as_str()
                    .and_then(|args| serde_json::from_str(args).ok())
                    .unwrap_or_else(|| json!({})),
            }),
            _ => {}
        }
    }
    if output.is_empty() {
        bail!(trf("Respuesta sin mensaje: {response}", &[("response", &response)]));
    }
    Ok(Turn { native: output, text, reasoning, tool_calls })
}

pub fn tool_result_messages(results: &[ToolResult]) -> Vec<Value> {
    results
        .iter()
        .map(|r| json!({ "type": "function_call_output", "call_id": r.call.id, "output": r.content }))
        .collect()
}
