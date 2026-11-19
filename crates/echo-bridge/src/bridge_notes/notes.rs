//! Scoped persistent notes. Every filesystem operation is relative to an opened
//! directory, never to a previously checked pathname. No payload diagnostics.
mod notes_files;
mod notes_paths;
mod notes_store;
#[cfg(test)]
mod notes_tests;

use crate::bridge_trace::Trace;
use echo_agent_api::{ToolError, ToolHost, ToolResult, ToolSpec};
use serde_json::{Value, json};
use std::path::PathBuf;

pub(crate) const NAMES: [&str; 5] = ["notes_list", "notes_read", "notes_write", "prompt_read", "prompt_write"];
const NOTE_BYTES: u64 = 65_536;
const TOTAL_BYTES: u64 = 16_777_216;
const TOTAL_FILES: usize = 1024;

#[derive(Clone)]
pub(crate) struct Notes {
    home: Option<PathBuf>,
    trace: Trace,
}

impl Notes {
    pub(crate) fn new(home: Option<&echo_config::HomeConfig>, trace: Trace) -> Self {
        Self {
            home: home.filter(|home| home.agent_tools.notes).map(|home| home.dir.clone()),
            trace,
        }
    }

    pub(crate) fn specs(&self) -> Vec<ToolSpec> {
        if self.home.is_none() {
            return vec![];
        }
        [
            ("notes_list", "List markdown notes, optionally beneath a relative directory.", json!({"directory":{"type":"string"}}), json!([])),
            ("notes_read", "Read a household markdown note.", json!({"path":{"type":"string"}}), json!(["path"])),
            ("notes_write", "Create or atomically replace a markdown note, at most 65536 UTF-8 bytes.", json!({"path":{"type":"string"},"content":{"type":"string"}}), json!(["path","content"])),
        ].into_iter().map(|(name, description, properties, required)| ToolSpec {
            name: name.into(), description: description.into(),
            input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
        }).collect()
    }

    pub(crate) fn call(&self, name: &str, args: &Value, key: &str) -> Result<ToolResult, ToolError> {
        // Only authored tool names enter receipts, including forged disabled calls.
        if !NAMES.contains(&name) {
            return Err(ToolError::Denied);
        }
        self.trace
            .clone()
            .tool_attempt(name, key)
            .map_err(|_| ToolError::Transport)?;
        let result = self.execute(name, args);
        let (content, path, bytes, code) = match result {
            Ok(receipt) => (receipt.content, receipt.path, receipt.bytes, "ok"),
            Err(code) => (json!({"error":code,"message":message(code)}), None, 0, code),
        };
        self.trace
            .clone()
            .agent_tool(name, path.as_deref(), bytes, code)
            .map_err(|_| ToolError::Transport)?;
        Ok(ToolResult {
            content,
            is_error: code != "ok",
        })
    }

    fn execute(&self, name: &str, args: &Value) -> Result<Receipt, &'static str> {
        let home = self
            .home
            .as_deref()
            .filter(|_| name.starts_with("notes_"))
            .ok_or("disabled")?;
        let request = notes_paths::Request::decode(name, args)?;
        notes_store::execute(home, request)
    }
}

struct Receipt {
    content: Value,
    path: Option<String>,
    bytes: u64,
}

fn message(code: &str) -> &'static str {
    match code {
        "invalid_arguments" => "Invalid tool arguments.",
        "disabled" => "This tool group is disabled.",
        "invalid_path" => "Use a relative markdown path inside notes.",
        "symlink" => "Symbolic links are not permitted.",
        "not_found" => "The note was not found.",
        "not_regular" => "Expected a private regular file or directory.",
        "note_too_large" => "A note may contain at most 65536 bytes.",
        "notes_bytes_limit" => "Notes may contain at most 16777216 bytes in total.",
        "notes_files_limit" => "Notes may contain at most 1024 files.",
        _ => "Notes storage is unavailable.",
    }
}

/// Local names never pass into MCP policy, including when disabled. The
/// dispatcher above this router reserves attempt IDs and owns deadlines/caps.
#[derive(Clone)]
pub(crate) struct Local<H> {
    pub(crate) host: H,
    pub(crate) notes: Notes,
}

impl<H: ToolHost + Sync> ToolHost for Local<H> {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>, ToolError> {
        self.host.list_tools().await
    }

    async fn call_tool(&self, name: &str, args: Value, key: &str) -> Result<ToolResult, ToolError> {
        if NAMES.contains(&name) {
            self.notes.call(name, &args, key)
        } else {
            self.host.call_tool(name, args, key).await
        }
    }
}
