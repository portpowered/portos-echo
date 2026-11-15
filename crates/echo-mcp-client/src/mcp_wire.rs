//! Bounded wire decoding, independent of HTTP.
use crate::McpError;
use echo_agent_api::{ToolResult, ToolSpec};
use serde::Deserialize;
use serde_json::Value;

pub(crate) const BODY_LIMIT: usize = 1_048_576;
pub(crate) const CATALOG_LIMIT: usize = 1024;
pub(crate) const PAGE_LIMIT: usize = 32;

pub(crate) fn envelope(bytes: &[u8], id: u64) -> Result<Value, McpError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| McpError::Protocol)?;
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || value.get("id").and_then(Value::as_u64) != Some(id)
        || value.get("result").is_some() == value.get("error").is_some()
    {
        return Err(McpError::Protocol);
    }
    if let Some(error) = value.get("error") {
        let code = error.get("code").and_then(Value::as_i64).ok_or(McpError::Protocol)?;
        if error.get("message").and_then(Value::as_str).is_none() {
            return Err(McpError::Protocol);
        }
        return Err(McpError::JsonRpc(code));
    }
    Ok(value["result"].clone())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireTool {
    name: String,
    description: String,
    input_schema: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    tools: Vec<WireTool>,
    next_cursor: Option<String>,
}

pub(crate) fn page(value: Value) -> Result<(Vec<ToolSpec>, Option<String>), McpError> {
    let page: Page = serde_json::from_value(value).map_err(|_| McpError::Protocol)?;
    if page.tools.len() > CATALOG_LIMIT {
        return Err(McpError::Capacity);
    }
    let mut tools = Vec::with_capacity(page.tools.len());
    for tool in page.tools {
        if tool.name.is_empty() || !tool.input_schema.is_object() {
            return Err(McpError::Protocol);
        }
        tools.push(ToolSpec {
            name: tool.name,
            description: tool.description,
            input_schema: tool.input_schema,
        });
    }
    Ok((tools, page.next_cursor))
}

pub(crate) fn result(value: &Value) -> Result<ToolResult, McpError> {
    let content = value
        .get("content")
        .filter(|v| v.is_array())
        .ok_or(McpError::Protocol)?;
    if !content
        .as_array()
        .is_some_and(|items| items.iter().all(Value::is_object))
    {
        return Err(McpError::Protocol);
    }
    let is_error = value
        .get("isError")
        .and_then(Value::as_bool)
        .ok_or(McpError::Protocol)?;
    Ok(ToolResult {
        content: content.clone(),
        is_error,
    })
}

/// Incremental SSE framing. Only a message event with one matching RPC response is accepted.
#[derive(Default)]
pub(crate) struct Sse {
    pending: Vec<u8>,
    data: String,
    event: String,
    used: usize,
}

impl Sse {
    pub(crate) fn push(&mut self, chunk: &[u8], id: u64) -> Result<Option<Value>, McpError> {
        self.used = self.used.checked_add(chunk.len()).ok_or(McpError::Capacity)?;
        if self.used > BODY_LIMIT {
            return Err(McpError::Capacity);
        }
        self.pending.extend_from_slice(chunk);
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let mut bytes: Vec<u8> = self.pending.drain(..=end).collect();
            bytes.pop();
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            let line = std::str::from_utf8(&bytes).map_err(|_| McpError::Protocol)?;
            if let Some(value) = self.line(line, id)? {
                return Ok(Some(value));
            }
        }
        Ok(None)
    }

    fn line(&mut self, line: &str, id: u64) -> Result<Option<Value>, McpError> {
        if line.is_empty() {
            if self.data.is_empty() {
                self.event.clear();
                return Ok(None);
            }
            if !self.event.is_empty() && self.event != "message" {
                return Err(McpError::Protocol);
            }
            let value = envelope(self.data.as_bytes(), id)?;
            self.data.clear();
            self.event.clear();
            return Ok(Some(value));
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => {
                self.data.push_str(value);
                self.data.push('\n');
            }
            "event" => value.clone_into(&mut self.event),
            _ => {}
        }
        Ok(None)
    }
}

/// Aggregate bounded pages without exposing partial discovery on exhaustion.
#[derive(Default)]
pub(crate) struct Catalog {
    tools: Vec<ToolSpec>,
    cursors: std::collections::HashSet<String>,
    pages: usize,
}
impl Catalog {
    pub(crate) fn add(&mut self, value: Value) -> Result<Option<String>, McpError> {
        let (page, cursor) = page(value)?;
        self.pages += 1;
        if self.tools.len() + page.len() > CATALOG_LIMIT || self.pages > PAGE_LIMIT {
            return Err(McpError::Capacity);
        }
        if let Some(cursor) = &cursor {
            if !self.cursors.insert(cursor.clone()) {
                return Err(McpError::Protocol);
            }
            if self.pages == PAGE_LIMIT {
                return Err(McpError::Capacity);
            }
        }
        self.tools.extend(page);
        Ok(cursor)
    }
    pub(crate) fn finish(self) -> Vec<ToolSpec> {
        self.tools
    }
}

pub(crate) fn initialized(result: &Value) -> Result<(), McpError> {
    if result["protocolVersion"] != "2025-03-26"
        || !result["capabilities"].is_object()
        || !result["serverInfo"]["name"].is_string()
        || !result["serverInfo"]["version"].is_string()
    {
        return Err(McpError::Protocol);
    }
    Ok(())
}
