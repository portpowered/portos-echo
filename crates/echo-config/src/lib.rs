//! Synchronous bridge configuration. Base loaders read only the nominated TOML file;
//! credential paths remain literal and are never opened or expanded.

mod config_auth;
mod config_clock;
mod config_codex;
mod config_decode;
mod config_muse;
mod config_screen;
mod config_settings;
mod config_validate;
mod config_voice;
pub use config_auth::ChatGptAuthConfig;
pub use config_clock::{ClockConfig, DEFAULT_TIME_SERVERS};
pub use config_codex::{
    CodexConfig, CodexLaunch, LIVE_INSTRUCTIONS, LIVE_SESSION_GUIDANCE, RUNNER_INSTRUCTIONS, RUNNER_SESSION_GUIDANCE,
    WebSearch, realtime_voices,
};
pub use config_muse::{AgentBackend, MuseConfig};
pub use config_screen::ScreenPackDownload;
pub use config_settings::{EffectiveConfig, SettingsOptions, SettingsOverlay, SettingsServerTools};
pub use config_voice::VoiceConfig;

#[cfg(test)]
mod config_rule_tests;

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

/// A diagnostic containing only a field path and a fixed category, never input values.
#[derive(Debug, thiserror::Error)]
#[error("{field}: {message}")]
pub struct ConfigError {
    field: String,
    message: String,
}

impl ConfigError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl BridgeConfig {
    /// Parse and validate TOML without reading credentials or contacting services.
    ///
    /// # Errors
    /// Returns a safe field-specific diagnostic for invalid configuration.
    pub fn from_toml_str(input: &str) -> Result<Self, ConfigError> {
        let table = toml::de::DeTable::parse(input).map_err(|error| {
            let offset = error.span().map_or(0, |span| span.start);
            ConfigError::new("config", format!("invalid TOML syntax at byte {offset}"))
        })?;
        let value = toml::de::DeValue::Table(table.into_inner());
        config_validate::reject_secrets(&value, "")?;
        let config = config_decode::decode(value)?;
        config_validate::validate(&config)?;
        Ok(config)
    }

    /// Read only `path`, then use the same validation as [`Self::from_toml_str`].
    /// Relative credential paths are preserved, including on repeated loads.
    ///
    /// # Errors
    /// Returns a safe config diagnostic for I/O/encoding errors, or a field error.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let input = std::fs::read_to_string(path)
            .map_err(|_| ConfigError::new("config", "cannot read UTF-8 configuration file"))?;
        Self::from_toml_str(&input)
    }
}

/// A fully validated result from a loader. Public fields may subsequently be mutated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeConfig {
    pub home: Option<HomeConfig>,
    pub wake_earcon: bool,
    pub chatgpt_auth: ChatGptAuthConfig,
    pub codex: CodexConfig,
    pub echo: EchoConfig,
    pub live: LiveConfig,
    pub voice: VoiceConfig,
    pub audio: AudioConfig,
    pub clock: ClockConfig,
    pub agent: AgentConfig,
    pub agent_backend: AgentBackend,
    pub muse: Option<MuseConfig>,
    pub mcp: McpConfig,
    pub screen: ScreenConfig,
    pub audit: AuditConfig,
    pub trace: TraceConfig,
}

/// Private delegation payload capture, applied when the bridge starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceConfig {
    pub payloads: bool,
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self { payloads: true }
    }
}

/// Operator-owned non-secret agent home; settings.toml is reserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeConfig {
    pub dir: PathBuf,
    pub agent_tools: AgentTools,
}

/// Independently selected bridge-local tool groups. A missing home remains inert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentTools {
    pub notes: bool,
    pub prompts: bool,
}

impl Default for AgentTools {
    fn default() -> Self {
        Self {
            notes: true,
            prompts: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EchoConfig {
    pub host: String,
    pub wyoming_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveConfig {
    pub mode: LiveMode,
    pub credential_file: PathBuf,
    pub voice: Option<String>,
    pub session_idle_seconds: u64,
}

/// Maximum reply audio sent ahead of playback, in complete 20 ms frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioConfig {
    pub reply_lead_ms: u16,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self { reply_lead_ms: 200 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveMode {
    Chatgpt,
    ApiKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentConfig {
    pub max_tool_calls: u32,
    pub task_deadline_seconds: u64,
    pub daily_request_cap: u64,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpConfig {
    pub server: Vec<McpServer>,
    pub state_dir: Option<PathBuf>,
    pub client_id: String,
    pub scopes: Vec<String>,
    pub operator_listen: SocketAddr,
}

/// The scopes enforced by Port OS MCP, in device-authorization request order.
pub const PORTOS_MCP_SCOPES: [&str; 7] = [
    "endpoint:read",
    "endpoint:write",
    "message:send",
    "group:read",
    "group:manage",
    "subscription:read",
    "subscription:write",
];

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            server: Vec::new(),
            state_dir: None,
            client_id: "port1/systems/zero/oauth-clients/command-line-interface".into(),
            scopes: PORTOS_MCP_SCOPES.map(String::from).into(),
            operator_listen: (std::net::Ipv4Addr::LOCALHOST, 16902).into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServer {
    pub name: String,
    pub url: String,
    pub auth: McpAuth,
    pub bearer_file: Option<PathBuf>,
    pub allow_tools: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpAuth {
    None,
    PortosDeviceCode,
    BearerFile,
}

impl McpAuth {
    pub(crate) fn default_tools(self) -> Vec<String> {
        if self != Self::PortosDeviceCode {
            return Vec::new();
        }
        [
            "control",
            "endpoint_query",
            "endpoint_show",
            "endpoint_manage",
            "group_query",
            "group_manage",
            "guide",
            "subscription_list",
            "subscription_inspect",
            "subscription_cancel",
            "subscription_label_edit",
        ]
        .map(String::from)
        .into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenConfig {
    pub listen: SocketAddr,
    pub pairing_token_file: PathBuf,
    /// Verified downloads, scheduled by the screen owner outside the voice path.
    pub packs: Vec<ScreenPackDownload>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditConfig {
    pub log_dir: PathBuf,
}
