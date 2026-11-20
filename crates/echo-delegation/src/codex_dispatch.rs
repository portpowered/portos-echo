//! App-server tool dispatch without a model client, process or Live socket.
//!
//! The single IO owner admits matching turn notifications, polls this work
//! alongside IO/media, and writes the returned result under the original RPC
//! request ID. It must deliver `interrupt` when daily admission is refused.
//! Dropping a dispatch future never permits its call ID to execute again.
//! The supplied `ToolHost` still owns confirmation policy and MCP authorization;
//! catalog discovery and model intent cannot grant consent.
mod catalog;
#[cfg(test)]
mod tests;
pub use catalog::Catalog;

use echo_agent_api::{ToolError, ToolHost};
use echo_codex::codex_protocol::{ToolCall, ToolContent, ToolResult, TurnInterrupt};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DispatchError {
    #[error("invalid tool dispatch configuration")]
    Configuration,
    #[error("invalid tool catalog")]
    Catalog,
    #[error("stale or duplicate tool request")]
    Stale,
    #[error("delegated turn budget exhausted")]
    Budget,
    #[error("tool request refused")]
    Refused,
}

/// Retain this counter across call generations. The owner supplies UTC day.
/// Check admission before bridge-owned model work. App-server-owned turns also
/// need a correlated interrupt when their observed admission is refused.
pub struct DailyBudget {
    cap: u64,
    day: Option<u64>,
    used: u64,
}

impl DailyBudget {
    #[must_use]
    pub fn new(cap: u64) -> Self {
        Self {
            cap,
            day: None,
            used: 0,
        }
    }

    /// # Errors
    /// Refuses an exhausted day or a clock moving to an earlier day.
    pub fn admit(&mut self, day: u64) -> Result<(), DispatchError> {
        self.check(day)?;
        self.used += 1;
        Ok(())
    }

    /// Check new-call admission without spending a delegated-turn allowance.
    /// # Errors
    /// Refuses an exhausted day or a clock moving to an earlier day.
    pub fn check(&mut self, day: u64) -> Result<(), DispatchError> {
        if self.day.is_some_and(|previous| day < previous) {
            return Err(DispatchError::Budget);
        }
        if self.day != Some(day) {
            self.day = Some(day);
            self.used = 0;
        }
        if self.used >= self.cap {
            return Err(DispatchError::Budget);
        }
        Ok(())
    }
}

struct Turn {
    id: String,
    calls: HashSet<String>,
    terminal: bool,
}

/// One instance per thread/process generation; never transfer it to a restart.
pub struct Dispatcher<H> {
    host: H,
    catalog: Catalog,
    thread: String,
    seen_turns: HashSet<String>,
    active: Option<Turn>,
}

impl<H: ToolHost> Dispatcher<H> {
    /// # Errors
    /// Invalid thread identity.
    pub fn new(host: H, catalog: Catalog, thread: String) -> Result<Self, DispatchError> {
        if !valid_id(&thread) {
            return Err(DispatchError::Configuration);
        }
        Ok(Self {
            host,
            catalog,
            thread,
            seen_turns: HashSet::new(),
            active: None,
        })
    }

    /// Bind a matching turn once.
    /// The IO owner must check daily admission and interrupt a refused turn.
    /// # Errors
    /// Invalid/foreign/duplicate turns, unfinished work or generation capacity.
    pub fn start_turn(&mut self, thread: &str, turn: &str) -> Result<(), DispatchError> {
        if thread != self.thread || !valid_id(turn) || self.active.is_some() || self.seen_turns.contains(turn) {
            return Err(DispatchError::Stale);
        }
        if self.seen_turns.len() == 1024 {
            return Err(DispatchError::Budget);
        }
        self.seen_turns.insert(turn.to_owned());
        self.active = Some(Turn {
            id: turn.into(),
            calls: HashSet::new(),
            terminal: false,
        });
        Ok(())
    }

    /// Complete only the active turn; stale completion cannot clear newer work.
    /// # Errors
    /// A foreign thread/turn is never treated as current completion.
    pub fn finish_turn(&mut self, thread: &str, turn: &str) -> Result<(), DispatchError> {
        if thread != self.thread || self.active.as_ref().is_none_or(|active| active.id != turn) {
            return Err(DispatchError::Stale);
        }
        self.active = None;
        Ok(())
    }

    /// Refuse the active turn before any tool effect and interrupt it once.
    pub fn refuse_budget(&mut self) -> Option<TurnInterrupt> {
        let turn = self.active.as_mut()?;
        if turn.terminal {
            return None;
        }
        turn.terminal = true;
        Some(TurnInterrupt {
            thread_id: self.thread.clone(),
            turn_id: turn.id.clone(),
        })
    }

    /// Execute at most one authorized attempt; never retry ambiguous effects.
    /// A matching server request ID remains the IO owner's responsibility.
    /// This future must be polled alongside IO, not block its event reader.
    pub async fn dispatch(&mut self, call: &ToolCall) -> ToolResult {
        let name = match self.admit(call) {
            Ok(name) => name,
            Err(error) => return failed(&error.to_string()),
        };
        let Some(turn) = &self.active else {
            return failed(&DispatchError::Stale.to_string());
        };
        // Length prefixes prevent ambiguous keys when opaque IDs contain colons.
        let key = format!(
            "{}:{}{}:{}{}:{}",
            self.thread.len(),
            self.thread,
            turn.id.len(),
            turn.id,
            call.call_id.len(),
            call.call_id
        );
        match Box::pin(self.host.call_tool(&name, call.arguments.clone(), &key)).await {
            Ok(result) => ToolResult {
                content_items: vec![ToolContent::InputText {
                    text: result.content.to_string(),
                }],
                success: !result.is_error,
            },
            Err(error) => failed(tool_error(&error)),
        }
    }

    fn admit(&mut self, call: &ToolCall) -> Result<String, DispatchError> {
        let turn = self.active.as_mut().ok_or(DispatchError::Stale)?;
        if call.thread_id != self.thread
            || call.turn_id != turn.id
            || !valid_id(&call.call_id)
            || turn.terminal
            || turn.calls.contains(&call.call_id)
        {
            return Err(DispatchError::Stale);
        }
        // Reserve even refused/cancelled requests, so they cannot be replayed.
        turn.calls.insert(call.call_id.clone());
        if call.namespace.is_some() || !call.arguments.is_object() {
            return Err(DispatchError::Refused);
        }
        self.catalog
            .resolve(&call.tool)
            .map(str::to_owned)
            .ok_or(DispatchError::Refused)
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn failed(text: &str) -> ToolResult {
    ToolResult {
        content_items: vec![ToolContent::InputText { text: text.into() }],
        success: false,
    }
}

fn tool_error(error: &ToolError) -> &'static str {
    match error {
        ToolError::Denied => "tool request denied",
        ToolError::Unauthorized => "tool authorization required",
        ToolError::Timeout => "tool deadline exceeded",
        ToolError::Transport | ToolError::Server(_) => "tool execution failed",
    }
}
