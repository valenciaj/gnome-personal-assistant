//! Clientes de LLM y bucle de agente.
//!
//! Cada conversación guarda el historial en el formato nativo de su proveedor
//! (`serde_json::Value`) y lo reenvía tal cual: así los bloques de razonamiento de
//! Claude vuelven sin modificar, que es lo que exige la API.

mod anthropic;
mod ollama;
mod openai;
mod openai_responses;

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::Value;

use crate::config::{LlmConfig, LlmProvider};
use crate::i18n::{tr, trf};
use crate::attachments::ModelImage;

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

/// Lo que devuelve una tool: datos para el modelo y una frase para el usuario.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub content: Value,
    pub summary: String,
}

impl ToolOutput {
    pub fn new(content: Value, summary: impl Into<String>) -> Self {
        Self { content, summary: summary.into() }
    }
}

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub call: ToolCall,
    /// Texto que recibe el modelo.
    pub content: String,
    /// Frase que ve el usuario.
    pub summary: String,
    pub is_error: bool,
}

/// Una respuesta del modelo ya interpretada.
#[derive(Debug)]
pub struct Turn {
    /// Lo que devuelve el modelo en formato nativo, para añadirlo al historial sin
    /// tocarlo (un mensaje; en la API Responses de OpenAI, varios elementos).
    pub native: Vec<Value>,
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
    pub vision: bool,
    http: reqwest::Client,
}

impl Client {
    /// Cliente del proveedor activo con su modelo configurado.
    pub fn from_config(config: &LlmConfig) -> Result<Self> {
        Self::new(config, config.provider, None)
    }

    /// Cliente de un proveedor concreto; `model` sustituye al configurado (para
    /// continuar una conversación con el modelo con el que empezó).
    pub fn new(config: &LlmConfig, provider: LlmProvider, model: Option<&str>) -> Result<Self> {
        let settings = config.settings(provider);
        let model = model.unwrap_or(&settings.model);
        if model.trim().is_empty() {
            bail!(trf("No hay modelo configurado para {provider}", &[("provider", &provider.label())]));
        }
        let api_key = config.api_key(provider);
        if api_key.is_empty() && provider != LlmProvider::Ollama {
            bail!(trf("Falta la API key de {provider}", &[("provider", &provider.label())]));
        }
        Ok(Self {
            provider,
            model: model.trim().to_string(),
            reasoning: settings.reasoning.trim().to_string(),
            base_url: settings.base_url.trim().trim_end_matches('/').to_string(),
            api_key,
            vision: settings.vision,
            http: http_client()?,
        })
    }

    /// Mensaje del usuario con sus imágenes, en el formato de cada API.
    pub fn user_message(&self, text: &str, images: &[ModelImage]) -> Value {
        use serde_json::json;
        if images.is_empty() {
            return json!({ "role": "user", "content": text });
        }
        if !self.vision {
            let note = trf(
                "[El usuario ha adjuntado {count} imágenes, pero este modelo no puede verlas.]",
                &[("count", &images.len())],
            );
            return json!({ "role": "user", "content": format!("{text}\n\n{note}") });
        }
        let data_url = |image: &ModelImage| format!("data:{};base64,{}", image.mime, image.base64);
        match self.provider {
            LlmProvider::Anthropic => {
                let mut content: Vec<Value> = images
                    .iter()
                    .map(|i| json!({ "type": "image", "source": { "type": "base64", "media_type": i.mime, "data": i.base64 } }))
                    .collect();
                content.push(json!({ "type": "text", "text": text }));
                json!({ "role": "user", "content": content })
            }
            LlmProvider::Openai => {
                let mut content = vec![json!({ "type": "input_text", "text": text })];
                content.extend(images.iter().map(|i| json!({ "type": "input_image", "image_url": data_url(i) })));
                json!({ "role": "user", "content": content })
            }
            LlmProvider::OpenaiCompatible => {
                let mut content = vec![json!({ "type": "text", "text": text })];
                content.extend(images.iter().map(|i| json!({ "type": "image_url", "image_url": { "url": data_url(i) } })));
                json!({ "role": "user", "content": content })
            }
            LlmProvider::Ollama => {
                json!({ "role": "user", "content": text, "images": images.iter().map(|i| i.base64.clone()).collect::<Vec<_>>() })
            }
        }
    }

    async fn complete(&self, system: &str, messages: &[Value], tools: &[ToolSpec]) -> Result<Turn> {
        match self.provider {
            LlmProvider::Anthropic => anthropic::complete(self, system, messages, tools).await,
            // OpenAI solo admite tools junto con razonamiento en la API Responses.
            LlmProvider::Openai => openai_responses::complete(self, system, messages, tools).await,
            LlmProvider::OpenaiCompatible => openai::complete(self, system, messages, tools).await,
            LlmProvider::Ollama => ollama::complete(self, system, messages, tools).await,
        }
    }

    fn tool_result_messages(&self, results: &[ToolResult]) -> Vec<Value> {
        match self.provider {
            LlmProvider::Anthropic => anthropic::tool_result_messages(results),
            LlmProvider::Openai => openai_responses::tool_result_messages(results),
            LlmProvider::OpenaiCompatible => openai::tool_result_messages(results),
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

/// Ejecuta las tools que pide el modelo.
pub trait ToolExecutor: Send + Sync {
    fn execute(&self, call: &ToolCall) -> impl Future<Output = Result<ToolOutput>> + Send;
}

const MAX_ITERATIONS: usize = 12;

/// Bucle de agente: llama al modelo, ejecuta las tools que pida y repite hasta que
/// responde sin pedir más. Añade todo al historial `messages` y lo que ve el usuario
/// a `display` (también lo ocurrido antes de un error).
pub async fn run_agent(
    client: &Client,
    system: &str,
    messages: &mut Vec<Value>,
    tools: &[ToolSpec],
    executor: &impl ToolExecutor,
    display: &mut Vec<DisplayItem>,
) -> Result<()> {
    for _ in 0..MAX_ITERATIONS {
        let turn = client.complete(system, messages, tools).await?;
        messages.extend(turn.native);
        if !turn.reasoning.trim().is_empty() {
            display.push(DisplayItem::Reasoning(turn.reasoning));
        }
        if !turn.text.trim().is_empty() {
            display.push(DisplayItem::Text(turn.text));
        }
        if turn.tool_calls.is_empty() {
            return Ok(());
        }
        // En orden: un borrado puede quedarse esperando la confirmación del usuario.
        let mut results = Vec::with_capacity(turn.tool_calls.len());
        for call in turn.tool_calls {
            let result = match executor.execute(&call).await {
                Ok(output) => ToolResult {
                    content: output.content.to_string(),
                    summary: output.summary,
                    is_error: false,
                    call,
                },
                Err(err) => {
                    let message = format!("{err:#}");
                    ToolResult { summary: format!("{}: {message}", call.name), content: message, is_error: true, call }
                }
            };
            log::info!("Tool {} → {}", result.call.name, result.summary);
            results.push(result);
        }
        messages.extend(client.tool_result_messages(&results));
        display.extend(results.into_iter().map(DisplayItem::Tool));
    }
    bail!(tr("El modelo ha encadenado demasiadas llamadas a tools sin terminar"))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// Servidor HTTP mínimo que responde en orden con `responses` y guarda los cuerpos recibidos.
    async fn mock_server(responses: Vec<Value>) -> (String, Arc<Mutex<Vec<Value>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let received = Arc::new(Mutex::new(Vec::new()));
        let log = received.clone();
        tokio::spawn(async move {
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = Vec::new();
                let body = loop {
                    let mut chunk = [0u8; 4096];
                    let n = socket.read(&mut chunk).await.unwrap();
                    buffer.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buffer).to_string();
                    if let Some(split) = text.find("\r\n\r\n") {
                        let length: usize = text[..split]
                            .lines()
                            .find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
                            .unwrap_or(0);
                        if buffer.len() >= split + 4 + length {
                            break buffer[split + 4..split + 4 + length].to_vec();
                        }
                    }
                };
                log.lock().unwrap().push(serde_json::from_slice(&body).unwrap());
                let payload = response.to_string();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                socket.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        (url, received)
    }

    struct Echo;

    impl ToolExecutor for Echo {
        async fn execute(&self, call: &ToolCall) -> Result<ToolOutput> {
            Ok(ToolOutput::new(json!({ "ok": call.input["text"] }), "hecho"))
        }
    }

    fn client(provider: LlmProvider, base_url: String) -> Client {
        Client {
            provider,
            model: "test".into(),
            reasoning: "default".into(),
            base_url,
            api_key: "k".into(),
            vision: true,
            http: http_client().unwrap(),
        }
    }

    fn tool() -> Vec<ToolSpec> {
        vec![ToolSpec { name: "add_todo".into(), description: "x".into(), schema: json!({"type": "object"}) }]
    }

    #[tokio::test]
    async fn anthropic_tool_loop() {
        let first = json!({
            "stop_reason": "tool_use",
            "content": [
                {"type": "thinking", "thinking": "pensando", "signature": "sig"},
                {"type": "tool_use", "id": "toolu_1", "name": "add_todo", "input": {"text": "pan"}}
            ]
        });
        let second = json!({ "stop_reason": "end_turn", "content": [{"type": "text", "text": "Añadido."}] });
        let (url, received) = mock_server(vec![first.clone(), second]).await;
        let client = client(LlmProvider::Anthropic, url);
        let mut messages = vec![client.user_message("apunta pan", &[])];
        let mut display = Vec::new();
        run_agent(&client, "sys", &mut messages, &tool(), &Echo, &mut display).await.unwrap();

        let requests = received.lock().unwrap();
        // El turno del asistente vuelve exactamente igual, con su bloque de razonamiento.
        assert_eq!(requests[1]["messages"][1]["content"], first["content"]);
        assert_eq!(
            requests[1]["messages"][2],
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "{\"ok\":\"pan\"}", "is_error": false}]})
        );
        assert_eq!(messages.len(), 4);
        assert!(matches!(&display[..], [DisplayItem::Reasoning(_), DisplayItem::Tool(_), DisplayItem::Text(t)] if t == "Añadido."));
    }

    #[tokio::test]
    async fn openai_compatible_tool_loop() {
        let first = json!({ "choices": [{ "message": {
            "role": "assistant", "content": null,
            "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "add_todo", "arguments": "{\"text\":\"pan\"}"}}]
        }}]});
        let second = json!({ "choices": [{ "message": { "role": "assistant", "content": "Añadido." }}]});
        let (url, received) = mock_server(vec![first, second]).await;
        let client = client(LlmProvider::OpenaiCompatible, url);
        let mut messages = vec![client.user_message("apunta pan", &[])];
        let mut display = Vec::new();
        run_agent(&client, "sys", &mut messages, &tool(), &Echo, &mut display).await.unwrap();

        let requests = received.lock().unwrap();
        assert_eq!(requests[0]["messages"][0], json!({"role": "system", "content": "sys"}));
        assert_eq!(requests[0]["tools"][0]["function"]["name"], "add_todo");
        assert_eq!(requests[1]["messages"][2]["tool_calls"][0]["id"], "call_1");
        assert_eq!(requests[1]["messages"][3], json!({"role": "tool", "tool_call_id": "call_1", "content": "{\"ok\":\"pan\"}"}));
        assert!(matches!(display.last(), Some(DisplayItem::Text(t)) if t == "Añadido."));
    }

    #[tokio::test]
    async fn openai_responses_tool_loop() {
        let reasoning = json!({"type": "reasoning", "id": "rs_1", "summary": [{"type": "summary_text", "text": "pensando"}], "encrypted_content": "cifrado"});
        let call = json!({"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "add_todo", "arguments": "{\"text\":\"pan\"}"});
        let first = json!({ "status": "completed", "output": [reasoning, call] });
        let second = json!({ "status": "completed", "output": [
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Añadido."}]}
        ]});
        let (url, received) = mock_server(vec![first, second]).await;
        let mut client = client(LlmProvider::Openai, url);
        client.reasoning = "low".into();
        let mut messages = vec![client.user_message("apunta pan", &[])];
        let mut display = Vec::new();
        run_agent(&client, "sys", &mut messages, &tool(), &Echo, &mut display).await.unwrap();

        let requests = received.lock().unwrap();
        assert_eq!(requests[0]["instructions"], "sys");
        assert_eq!(requests[0]["store"], false);
        assert_eq!(requests[0]["reasoning"], json!({"effort": "low", "summary": "auto"}));
        assert_eq!(requests[0]["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(requests[0]["tools"][0], json!({"type": "function", "name": "add_todo", "description": "x", "parameters": {"type": "object"}}));
        // Razonamiento cifrado y llamada vuelven tal cual, seguidos del resultado.
        assert_eq!(requests[1]["input"][1], reasoning);
        assert_eq!(requests[1]["input"][2], call);
        assert_eq!(requests[1]["input"][3], json!({"type": "function_call_output", "call_id": "call_1", "output": "{\"ok\":\"pan\"}"}));
        assert!(matches!(&display[..], [DisplayItem::Reasoning(r), DisplayItem::Tool(_), DisplayItem::Text(t)] if r == "pensando" && t == "Añadido."));
    }

    #[test]
    fn user_messages_with_images() {
        let image = ModelImage { mime: "image/jpeg".into(), base64: "QUJD".into() };
        let images = [image];
        let message = |provider, vision| {
            let mut c = client(provider, String::new());
            c.vision = vision;
            c.user_message("mira", &images)
        };
        assert_eq!(message(LlmProvider::Anthropic, true)["content"][0]["source"]["data"], "QUJD");
        assert_eq!(message(LlmProvider::Anthropic, true)["content"][1]["text"], "mira");
        assert_eq!(message(LlmProvider::Openai, true)["content"][1]["image_url"], "data:image/jpeg;base64,QUJD");
        assert_eq!(message(LlmProvider::OpenaiCompatible, true)["content"][1]["image_url"]["url"], "data:image/jpeg;base64,QUJD");
        assert_eq!(message(LlmProvider::Ollama, true)["images"][0], "QUJD");
        let blind = message(LlmProvider::Ollama, false);
        assert!(blind["content"].as_str().unwrap().starts_with("mira\n\n[") && blind.get("images").is_none());
    }
}
