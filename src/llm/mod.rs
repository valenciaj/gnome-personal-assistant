//! Clientes de LLM y bucle de agente.
//!
//! Cada conversación guarda el historial en el formato nativo de su proveedor
//! (`serde_json::Value`) y lo reenvía tal cual: así los bloques de razonamiento de
//! Claude vuelven sin modificar, que es lo que exige la API.

mod anthropic;
mod ollama;
mod openai;

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::Value;

use crate::config::{LlmConfig, LlmProvider};

/// Definición de una tool en formato neutro (JSON Schema).
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub call: ToolCall,
    pub content: String,
    pub is_error: bool,
}

/// Una respuesta del modelo ya interpretada.
#[derive(Debug)]
pub struct Turn {
    /// Mensaje del asistente en formato nativo, para añadirlo al historial sin tocarlo.
    pub native: Value,
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
}

/// Elementos que se muestran al usuario en el popup.
#[derive(Debug, Clone)]
pub enum DisplayItem {
    Reasoning(String),
    Text(String),
    Tool(ToolResult),
}

/// Configuración resuelta de un proveedor para una petición.
#[derive(Debug, Clone)]
pub struct Client {
    pub provider: LlmProvider,
    pub model: String,
    pub reasoning: String,
    pub base_url: String,
    pub api_key: String,
    http: reqwest::Client,
}

impl Client {
    pub fn from_config(config: &LlmConfig) -> Result<Self> {
        let provider = config.provider;
        let settings = config.settings(provider);
        if settings.model.trim().is_empty() {
            bail!("No hay modelo configurado para {}", provider.label());
        }
        let api_key = config.api_key(provider);
        if api_key.is_empty() && provider != LlmProvider::Ollama {
            bail!("Falta la API key de {}", provider.label());
        }
        Ok(Self {
            provider,
            model: settings.model.trim().to_string(),
            reasoning: settings.reasoning.trim().to_string(),
            base_url: settings.base_url.trim().trim_end_matches('/').to_string(),
            api_key,
            http: http_client()?,
        })
    }

    pub fn user_message(&self, text: &str) -> Value {
        serde_json::json!({ "role": "user", "content": text })
    }

    async fn complete(&self, system: &str, messages: &[Value], tools: &[ToolSpec]) -> Result<Turn> {
        match self.provider {
            LlmProvider::Anthropic => anthropic::complete(self, system, messages, tools).await,
            LlmProvider::Openai | LlmProvider::OpenaiCompatible => {
                openai::complete(self, system, messages, tools).await
            }
            LlmProvider::Ollama => ollama::complete(self, system, messages, tools).await,
        }
    }

    fn tool_result_messages(&self, results: &[ToolResult]) -> Vec<Value> {
        match self.provider {
            LlmProvider::Anthropic => anthropic::tool_result_messages(results),
            LlmProvider::Openai | LlmProvider::OpenaiCompatible => {
                openai::tool_result_messages(results)
            }
            LlmProvider::Ollama => ollama::tool_result_messages(results),
        }
    }

    pub async fn list_models(&self) -> Result<Vec<String>> {
        let mut models = match self.provider {
            LlmProvider::Anthropic => anthropic::list_models(self).await?,
            LlmProvider::Openai | LlmProvider::OpenaiCompatible => openai::list_models(self).await?,
            LlmProvider::Ollama => ollama::list_models(self).await?,
        };
        models.sort();
        Ok(models)
    }

    fn reasoning_level(&self) -> Option<&str> {
        match self.reasoning.as_str() {
            "" | "default" => None,
            level => Some(level),
        }
    }
}

pub fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(600))
        .build()?)
}

/// Comprueba el estado HTTP y devuelve el cuerpo JSON, o un error con el mensaje de la API.
pub(crate) async fn json_response(response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let body = response.text().await?;
    if status.is_success() {
        return Ok(serde_json::from_str(&body)?);
    }
    let detail = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.get("error"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or(body);
    Err(anyhow!("HTTP {status}: {detail}"))
}

/// Ejecuta una tool. La aporta quien llama al agente (en la fase 2, la base de datos).
pub trait ToolExecutor: Send + Sync {
    fn execute(&self, call: &ToolCall) -> Result<String>;
}

pub struct NoTools;

impl ToolExecutor for NoTools {
    fn execute(&self, call: &ToolCall) -> Result<String> {
        bail!("La tool {} no existe", call.name)
    }
}

const MAX_ITERATIONS: usize = 12;

/// Bucle de agente: llama al modelo, ejecuta las tools que pida y repite hasta que
/// responde sin pedir más. Añade todo al historial `messages`.
pub async fn run_agent(
    client: &Client,
    system: &str,
    messages: &mut Vec<Value>,
    tools: &[ToolSpec],
    executor: &dyn ToolExecutor,
) -> Result<Vec<DisplayItem>> {
    let mut display = Vec::new();
    for _ in 0..MAX_ITERATIONS {
        let turn = client.complete(system, messages, tools).await?;
        messages.push(turn.native);
        if !turn.reasoning.trim().is_empty() {
            display.push(DisplayItem::Reasoning(turn.reasoning));
        }
        if !turn.text.trim().is_empty() {
            display.push(DisplayItem::Text(turn.text));
        }
        if turn.tool_calls.is_empty() {
            return Ok(display);
        }
        let results: Vec<ToolResult> = turn
            .tool_calls
            .into_iter()
            .map(|call| match executor.execute(&call) {
                Ok(content) => ToolResult { call, content, is_error: false },
                Err(err) => ToolResult { call, content: err.to_string(), is_error: true },
            })
            .collect();
        messages.extend(client.tool_result_messages(&results));
        display.extend(results.into_iter().map(DisplayItem::Tool));
    }
    bail!("El modelo ha encadenado demasiadas llamadas a tools sin terminar")
}
