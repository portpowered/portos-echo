//! Safe categories; never retain remote text or transport error chains.
use echo_agent_api::ToolError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum McpError {
    #[error("tool refused")]
    Refused,
    #[error("token unavailable")]
    TokenUnavailable,
    #[error("invalid configuration")]
    InvalidConfiguration,
    #[error("HTTP status {0}")]
    HttpStatus(u16),
    #[error("JSON-RPC code {0}")]
    JsonRpc(i64),
    #[error("invalid protocol response")]
    Protocol,
    #[error("deadline exceeded")]
    Timeout,
    #[error("transport failure")]
    Transport,
    #[error("capacity exhausted")]
    Capacity,
}

impl From<McpError> for ToolError {
    fn from(error: McpError) -> Self {
        match error {
            McpError::Refused => Self::Denied,
            McpError::TokenUnavailable | McpError::HttpStatus(401 | 403) => Self::Unauthorized,
            McpError::Timeout => Self::Timeout,
            McpError::HttpStatus(n) => Self::Server(format!("http_status:{n}")),
            McpError::JsonRpc(n) => Self::Server(format!("json_rpc:{n}")),
            McpError::Protocol | McpError::Transport => Self::Transport,
            McpError::InvalidConfiguration => Self::Server("invalid_configuration".into()),
            McpError::Capacity => Self::Server("capacity".into()),
        }
    }
}
