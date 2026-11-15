//! Synchronous bridge configuration. Loaders read only the nominated TOML file;
//! credential paths remain literal and are never opened or expanded.

mod config_auth;
mod config_decode;
mod config_validate;
pub use config_auth::ChatGptAuthConfig;

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
    pub chatgpt_auth: ChatGptAuthConfig,
    pub echo: EchoConfig,
    pub live: LiveConfig,
    pub agent: AgentConfig,
    pub mcp: McpConfig,
    pub screen: ScreenConfig,
    pub audit: AuditConfig,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenConfig {
    pub listen: SocketAddr,
    pub pairing_token_file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditConfig {
    pub log_dir: PathBuf,
}
