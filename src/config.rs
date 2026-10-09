//! Configuración en TOML (~/.config/personal-assistant/config.toml).
//!
//! Provisional hasta la fase 4, en la que se mueve a GSettings + llavero.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::i18n::{n_, tr};

pub const APP_ID: &str = "io.github.valenciaj.PersonalAssistant";
const DIR_NAME: &str = "personal-assistant";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmProvider {
    Anthropic,
    Openai,
    Ollama,
    OpenaiCompatible,
}

const ANTHROPIC_LEVELS: &[(&str, &str)] = &[
                ("default", n_("Por defecto del modelo")),
                ("low", n_("Bajo")),
                ("medium", n_("Medio")),
                ("high", n_("Alto")),
                ("xhigh", n_("Muy alto")),
                ("max", n_("Máximo")),
            ];
const OPENAI_LEVELS: &[(&str, &str)] = &[
                ("default", n_("Por defecto del modelo")),
                ("none", n_("Sin razonamiento")),
                ("minimal", n_("Mínimo")),
                ("low", n_("Bajo")),
                ("medium", n_("Medio")),
                ("high", n_("Alto")),
                ("xhigh", n_("Muy alto")),
            ];
const OLLAMA_LEVELS: &[(&str, &str)] = &[
                ("default", n_("Por defecto del modelo")),
                ("off", n_("Desactivado")),
                ("on", n_("Activado")),
                ("low", n_("Bajo (gpt-oss)")),
                ("medium", n_("Medio (gpt-oss)")),
                ("high", n_("Alto (gpt-oss)")),
            ];

impl LlmProvider {
    pub const ALL: [Self; 4] = [Self::Anthropic, Self::Openai, Self::Ollama, Self::OpenaiCompatible];

    /// Identificador estable que se guarda en la base de datos.
    pub fn id(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
            Self::Ollama => "ollama",
            Self::OpenaiCompatible => "openai_compatible",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == id)
    }

    /// Niveles de razonamiento que admite cada API: (valor, etiqueta sin traducir).
    pub fn reasoning_levels(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Anthropic => ANTHROPIC_LEVELS,
            Self::Openai | Self::OpenaiCompatible => OPENAI_LEVELS,
            Self::Ollama => OLLAMA_LEVELS,
        }
    }

    /// Nombre de su API key en el llavero.
    pub fn secret_name(self) -> String {
        format!("llm-{}", self.id())
    }

    pub fn label(self) -> String {
        match self {
            Self::Anthropic => "Anthropic".into(),
            Self::Openai => "OpenAI".into(),
            Self::Ollama => "Ollama".into(),
            Self::OpenaiCompatible => tr("Compatible con OpenAI"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SttProvider {
    Openai,
    OpenaiCompatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingMode {
    /// Pulsar para empezar, pulsar para parar.
    Toggle,
    /// Grabar mientras se mantiene pulsado el atajo.
    PushToTalk,
    /// Pulsar para empezar; para solo al detectar silencio.
    AutoStop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderSettings {
    pub model: String,
    /// "default" omite el parámetro. Valores según proveedor:
    /// Anthropic: low|medium|high|xhigh|max · OpenAI/compatible: none|minimal|low|medium|high|xhigh
    /// · Ollama: off|on|low|medium|high
    pub reasoning: String,
    pub base_url: String,
    /// Si está vacío se usa la variable de entorno del proveedor.
    pub api_key: String,
    /// El modelo acepta imágenes; si no, solo se le dice que hay imágenes adjuntas.
    pub vision: bool,
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            model: String::new(),
            reasoning: "default".into(),
            base_url: String::new(),
            api_key: String::new(),
            vision: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub provider: LlmProvider,
    pub anthropic: ProviderSettings,
    pub openai: ProviderSettings,
    pub ollama: ProviderSettings,
    pub openai_compatible: ProviderSettings,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: LlmProvider::Anthropic,
            anthropic: ProviderSettings {
                model: "claude-opus-5-5".into(),
                reasoning: "medium".into(),
                base_url: "https://api.anthropic.com".into(),
                vision: true,
                ..Default::default()
            },
            openai: ProviderSettings {
                model: "gpt-5".into(),
                reasoning: "low".into(),
                base_url: "https://api.openai.com/v1".into(),
                vision: true,
                ..Default::default()
            },
            ollama: ProviderSettings {
                model: "qwen3".into(),
                reasoning: "default".into(),
                base_url: "http://localhost:11434".into(),
                ..Default::default()
            },
            openai_compatible: ProviderSettings {
                base_url: "https://openrouter.ai/api/v1".into(),
                ..Default::default()
            },
        }
    }
}

impl LlmConfig {
    pub fn settings(&self, provider: LlmProvider) -> &ProviderSettings {
        match provider {
            LlmProvider::Anthropic => &self.anthropic,
            LlmProvider::Openai => &self.openai,
            LlmProvider::Ollama => &self.ollama,
            LlmProvider::OpenaiCompatible => &self.openai_compatible,
        }
    }

    pub fn settings_mut(&mut self, provider: LlmProvider) -> &mut ProviderSettings {
        match provider {
            LlmProvider::Anthropic => &mut self.anthropic,
            LlmProvider::Openai => &mut self.openai,
            LlmProvider::Ollama => &mut self.ollama,
            LlmProvider::OpenaiCompatible => &mut self.openai_compatible,
        }
    }

    /// Clave de un proveedor: llavero, archivo de configuración o variable de entorno.
    pub fn api_key(&self, provider: LlmProvider) -> String {
        if let Some(key) = crate::secrets::get(&provider.secret_name()) {
            return key;
        }
        let configured = &self.settings(provider).api_key;
        if !configured.is_empty() {
            return configured.clone();
        }
        let var = match provider {
            LlmProvider::Anthropic => "ANTHROPIC_API_KEY",
            LlmProvider::Openai => "OPENAI_API_KEY",
            LlmProvider::Ollama => return String::new(),
            LlmProvider::OpenaiCompatible => "OPENAI_COMPATIBLE_API_KEY",
        };
        std::env::var(var).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SttConfig {
    pub provider: SttProvider,
    pub model: String,
    pub base_url: String,
    pub api_key: String,
    /// Código ISO-639-1 ("es", "en"…). Vacío = autodetectar.
    pub language: String,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            provider: SttProvider::Openai,
            model: "gpt-4o-transcribe".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_key: String::new(),
            language: String::new(),
        }
    }
}

pub const STT_SECRET: &str = "stt";

impl SttConfig {
    /// Clave propia de la transcripción o, si no hay, la de OpenAI del LLM.
    pub fn api_key(&self) -> String {
        crate::secrets::get(STT_SECRET)
            .or_else(|| (!self.api_key.is_empty()).then(|| self.api_key.clone()))
            .or_else(|| crate::secrets::get(&LlmProvider::Openai.secret_name()))
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordingConfig {
    pub mode: RecordingMode,
    /// Nivel RMS (dBFS) por encima del cual se considera que hay voz.
    pub voice_threshold_db: f64,
    /// Segundos de silencio tras la voz que paran la grabación en modo auto_stop.
    pub silence_seconds: f64,
    /// Duración máxima de una grabación en cualquier modo.
    pub max_seconds: u32,
    /// Nodo de PipeWire del micrófono (`node.name`); vacío = el predeterminado.
    pub device: String,
    /// Pitidos al empezar y al parar.
    pub sounds: bool,
    /// Ventanita con el tiempo y el nivel mientras se graba.
    pub indicator: bool,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            mode: RecordingMode::Toggle,
            voice_threshold_db: -45.0,
            silence_seconds: 2.0,
            max_seconds: 300,
            device: String::new(),
            sounds: true,
            indicator: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioRetention {
    /// Se conserva solo el audio que la IA adjunta a una nota.
    WithNotes,
    /// Se conservan todas las grabaciones en el historial de conversaciones.
    Always,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    pub retention: AudioRetention,
    /// Días tras los que se borran los audios que no están en una nota (0 = nunca).
    pub delete_after_days: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self { retention: AudioRetention::WithNotes, delete_after_days: 30 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RemindersConfig {
    pub enabled: bool,
    /// Hora ('HH:MM') del aviso de las tareas que solo tienen día (y de «mañana» al posponer).
    pub default_time: String,
    /// Opciones de posponer: "10m", "2h", "1d" o "tomorrow". La primera va en la notificación.
    pub snooze: Vec<String>,
    /// Repetir la alarma hasta que se atienda el recordatorio (como mucho 2 minutos).
    pub repeat_alarm: bool,
}

impl Default for RemindersConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_time: "09:00".into(),
            snooze: vec!["10m".into(), "1h".into(), "tomorrow".into()],
            repeat_alarm: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Idioma de la interfaz: "auto", "es" o "en".
    pub language: String,
    /// Se pone a true tras el primer arranque (en el que se activa el autoarranque).
    pub first_run_done: bool,
    pub system_prompt: String,
    pub stt: SttConfig,
    pub llm: LlmConfig,
    pub recording: RecordingConfig,
    pub audio: AudioConfig,
    pub reminders: RemindersConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: "auto".into(),
            first_run_done: false,
            system_prompt: DEFAULT_SYSTEM_PROMPT.into(),
            stt: SttConfig::default(),
            llm: LlmConfig::default(),
            recording: RecordingConfig::default(),
            audio: AudioConfig::default(),
            reminders: RemindersConfig::default(),
        }
    }
}

pub const DEFAULT_SYSTEM_PROMPT: &str = "\
Eres un asistente personal de escritorio. El usuario te habla por voz, así que \
sus mensajes son transcripciones y pueden contener errores de reconocimiento: \
interpreta la intención más probable. Responde en el mismo idioma que el usuario, \
de forma breve y directa; se leerá en una ventana pequeña. Puedes usar Markdown.";

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(DIR_NAME)
        .join("config.toml")
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(DIR_NAME)
}

pub fn audio_dir() -> PathBuf {
    data_dir().join("audio")
}

pub fn images_dir() -> PathBuf {
    data_dir().join("images")
}

impl Config {
    /// Carga la configuración, creando el archivo con valores por defecto si no existe.
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            let config = Self::default();
            config.save()?;
            log::info!("Configuración creada en {}", path.display());
            return Ok(config);
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("No se pudo leer {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("Error de sintaxis en {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}
