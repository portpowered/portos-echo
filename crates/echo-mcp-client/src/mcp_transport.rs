//! HTTP handling with bounded reads and deliberately opaque diagnostics.
use crate::{McpClient, McpError, TokenProvider, mcp_wire};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderValue, WWW_AUTHENTICATE};
use serde_json::Value;

impl<P: TokenProvider> McpClient<P> {
    pub(crate) async fn post(&self, body: Value, id: Option<u64>) -> Result<Value, McpError> {
        let tool_call = body["method"] == "tools/call";
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
        if status == 401 {
            self.provider.unauthorized();
        }
        tracing::trace!(status, "MCP response");
        if status == 403 && tool_call {
            let mut headers = response.headers().get_all(WWW_AUTHENTICATE).iter();
            if let Some(scope) = headers.next().and_then(scope_challenge)
                && headers.next().is_none()
            {
                return Err(McpError::InsufficientScope { scope });
            }
        }
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

fn scope_challenge(header: &HeaderValue) -> Option<&'static str> {
    let header = header.to_str().ok()?;
    if header.len() > 1024 {
        return None;
    }
    let (scheme, mut params) = header.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }
    let (mut error, mut scope) = (None, None);
    loop {
        let (name, remaining) = params.split_once('=')?;
        let name = name.trim();
        if name.is_empty() || !name.bytes().all(http_token) {
            return None;
        }
        let (value, rest) = challenge_value(remaining.trim_start())?;
        let target = if name.eq_ignore_ascii_case("error") {
            Some(&mut error)
        } else if name.eq_ignore_ascii_case("scope") {
            Some(&mut scope)
        } else {
            None
        };
        if let Some(target) = target
            && target.replace(value).is_some()
        {
            return None;
        }
        if rest.trim_start().is_empty() {
            break;
        }
        params = rest.trim_start().strip_prefix(',')?;
    }
    if error != Some("insufficient_scope") {
        return None;
    }
    [
        "endpoint:read",
        "endpoint:write",
        "message:send",
        "group:read",
        "group:manage",
        "subscription:read",
        "subscription:write",
    ]
    .into_iter()
    .find(|known| Some(*known) == scope)
}

fn challenge_value(input: &str) -> Option<(&str, &str)> {
    if let Some(quoted) = input.strip_prefix('"') {
        let mut escaped = false;
        for (index, byte) in quoted.bytes().enumerate() {
            if !escaped && byte == b'"' {
                return Some((&quoted[..index], &quoted[index + 1..]));
            }
            escaped = !escaped && byte == b'\\';
        }
        return None;
    }
    let end = input.bytes().take_while(|byte| http_token(*byte)).count();
    (end != 0).then(|| input.split_at(end))
}

fn http_token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^`_|~".contains(&byte)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_single_known_scope_in_a_valid_bearer_challenge_is_retained() {
        for text in [
            r#"Bearer error="insufficient_scope", scope="message:send""#,
            r#"bearer realm="ignored, with \"quotes\"", Scope="message:send", ERROR=insufficient_scope"#,
        ] {
            assert_eq!(
                scope_challenge(&HeaderValue::from_str(text).unwrap()),
                Some("message:send")
            );
        }
        for text in [
            r#"Basic error="insufficient_scope", scope="message:send""#,
            r#"Bearer error_description="insufficient_scope", scope="message:send""#,
            r#"Bearer error="invalid_token", scope="message:send""#,
            r#"Bearer error="insufficient_scope", scope="message:send endpoint:read""#,
            r#"Bearer error="insufficient_scope", scope="unrecognized:scope""#,
            r#"Bearer error="insufficient_scope", scope="message:send", scope="endpoint:read""#,
            r#"Bearer error="insufficient_scope", scope="message:send"#,
            r#"Bearer error="insufficient_scope", scope="message:send","#,
        ] {
            assert_eq!(scope_challenge(&HeaderValue::from_str(text).unwrap()), None, "{text}");
        }
    }
}
