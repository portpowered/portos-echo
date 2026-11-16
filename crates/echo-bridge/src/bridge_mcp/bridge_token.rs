use super::Grant;
use echo_config::{McpAuth, McpServer};
use echo_contracts::secret::Secret;
use echo_mcp_auth::TokenProvider as _;
use echo_mcp_client::{TokenError, TokenProvider};
use std::sync::Arc;

pub(crate) struct Token {
    server: McpServer,
    grant: Option<Arc<Grant>>,
}

impl Token {
    pub fn new(server: McpServer, grant: Option<Arc<Grant>>) -> Self {
        Self { server, grant }
    }

    async fn acquire(&self) -> Result<Secret<String>, TokenError> {
        match self.server.auth {
            McpAuth::None => Ok(Secret::new(String::new())),
            McpAuth::BearerFile => bearer(self.server.bearer_file.as_deref().ok_or(TokenError::Unavailable)?),
            McpAuth::PortosDeviceCode => {
                let grant = self.grant.as_ref().ok_or(TokenError::Unavailable)?;
                if grant.blocked() {
                    return Err(TokenError::Unavailable);
                }
                let token = grant.client.access_token().await.map_err(|error| {
                    grant.reject(&error.to_string());
                    TokenError::Unavailable
                })?;
                if grant.blocked() {
                    return Err(TokenError::Unavailable);
                }
                grant.used().map_err(|_| TokenError::Unavailable)?;
                Ok(token)
            }
        }
    }
}

impl TokenProvider for Token {
    async fn token(&self) -> Result<Secret<String>, TokenError> {
        self.acquire().await
    }

    fn unauthorized(&self) {
        if let Some(grant) = &self.grant {
            grant.unauthorized();
        }
    }
}

fn bearer(path: &std::path::Path) -> Result<Secret<String>, TokenError> {
    let file = std::fs::File::open(path).map_err(|_| TokenError::Unavailable)?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(file, 16_385), &mut bytes)
        .map_err(|_| TokenError::Unavailable)?;
    let text = String::from_utf8(bytes).map_err(|_| TokenError::Unavailable)?;
    let key = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(&text);
    if key.len() > 16_384 || key.trim().is_empty() || key.chars().any(char::is_control) {
        return Err(TokenError::Unavailable);
    }
    Ok(Secret::new(key.into()))
}
