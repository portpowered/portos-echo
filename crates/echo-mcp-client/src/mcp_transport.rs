//! HTTP handling with bounded reads and deliberately opaque diagnostics.
use crate::{McpClient, McpError, TokenProvider, mcp_wire};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use serde_json::Value;

impl<P: TokenProvider> McpClient<P> {
    pub(crate) async fn post(&self, body: Value, id: Option<u64>) -> Result<Value, McpError> {
        let bytes = serde_json::to_vec(&body).map_err(|_| McpError::Protocol)?;
        if bytes.len() > mcp_wire::BODY_LIMIT {
            return Err(McpError::Capacity);
        }
        let token = self.provider.token().await.map_err(|_| McpError::TokenUnavailable)?;
        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {}", token.expose())).map_err(|_| McpError::TokenUnavailable)?;
        authorization.set_sensitive(true);
        let mut response = self
            .http
            .post(self.endpoint.clone())
            .header(AUTHORIZATION, authorization)
            .header(ACCEPT, "application/json, text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .header("MCP-Protocol-Version", "2025-03-26")
            .body(bytes)
            .send()
            .await
            .map_err(|_| McpError::Transport)?;
        let status = response.status().as_u16();
        tracing::trace!(status, "MCP response");
        if let Some(id) = id {
            if status != 200 {
                return Err(McpError::HttpStatus(status));
            }
            return read(&mut response, id).await;
        }
        if status != 202 {
            return Err(McpError::HttpStatus(status));
        }
        if response
            .chunk()
            .await
            .map_err(|_| McpError::Transport)?
            .is_some_and(|b| !b.is_empty())
        {
            return Err(McpError::Protocol);
        }
        Ok(Value::Null)
    }
}

async fn read(response: &mut reqwest::Response, id: u64) -> Result<Value, McpError> {
    let media = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(str::trim)
        .ok_or(McpError::Protocol)?;
    let sse = match media {
        "text/event-stream" => true,
        "application/json" => false,
        _ => return Err(McpError::Protocol),
    };
    if response
        .content_length()
        .is_some_and(|n| n > mcp_wire::BODY_LIMIT as u64)
    {
        return Err(McpError::Capacity);
    }
    let mut bytes = Vec::new();
    let mut parser = mcp_wire::Sse::default();
    while let Some(chunk) = response.chunk().await.map_err(|_| McpError::Transport)? {
        if sse {
            if let Some(value) = parser.push(&chunk, id)? {
                return Ok(value);
            }
        } else {
            if bytes.len() + chunk.len() > mcp_wire::BODY_LIMIT {
                return Err(McpError::Capacity);
            }
            bytes.extend_from_slice(&chunk);
        }
    }
    if sse {
        return Err(McpError::Protocol);
    }
    mcp_wire::envelope(&bytes, id)
}
