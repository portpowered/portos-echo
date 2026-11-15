use std::{net::SocketAddr, path::PathBuf};

/// A fully validated result from a loader. Public fields may subsequently be mutated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeConfig {
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
