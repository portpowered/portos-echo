//! Run-scoped answers to bridge-owned session tools. Only this run owner
//! mutates waiting or end state; every settled invocation writes one receipt
//! before its result is released, so a failed receipt never reports success.
use super::{Error, Notice, session_controller::Controller};
use crate::bridge_codex::{SessionCommand, SessionRequest};
use echo_agent_api::ToolResult;
use serde_json::json;
use tokio::time::{Duration, Instant};

pub(super) const EXTEND: &str = "extend_session";
pub(super) const END: &str = "end_session";
/// Bounded idempotency memory; a run admits far fewer tool calls than this.
const ANSWERED: usize = 64;

/// Payload-free projection of one settled session tool invocation.
pub struct Receipt {
    pub tool: &'static str,
    pub outcome: &'static str,
    pub deadline_ms: Option<u64>,
    pub capped: Option<bool>,
}

/// Grants belong to one admitted run; a fresh run starts without them.
#[derive(Default)]
pub(super) struct Grants {
    opened: Option<Instant>,
    listen: Option<Duration>,
    max: Duration,
    granted: Option<Instant>,
    ending: bool,
    answered: Vec<(String, ToolResult)>,
}

impl Grants {
    pub fn new(listen: Option<Duration>, max: Duration) -> Self {
        Self {
            opened: Some(Instant::now()),
            listen,
            max,
            ..Self::default()
        }
    }

    /// Sticky after-play end requested by the agent.
    pub fn ending(&self) -> bool {
        self.ending
    }

    /// Tool-granted absolute listen deadline, retained across replies.
    pub fn granted(&self) -> Option<Instant> {
        self.granted
    }

    /// Grant `seconds` beyond `base`, never past the run's opening plus max.
    /// The base already includes any earlier grant, so a grant never shrinks.
    fn extend(&mut self, base: Instant, seconds: u64, opened: Instant) -> (Instant, bool) {
        let cap = opened.checked_add(self.max).unwrap_or(base);
        let wanted = base.checked_add(Duration::from_secs(seconds));
        let capped = wanted.is_none_or(|wanted| wanted > cap);
        let deadline = wanted.map_or(cap, |wanted| wanted.min(cap));
        self.granted = Some(deadline);
        (deadline, capped)
    }
}

/// Answer one admitted request for the active run.
/// # Errors
/// A failed receipt drops the reply, so the tool fails instead of succeeding.
pub(super) fn answer(
    request: SessionRequest,
    grants: &mut Grants,
    controller: &mut Controller,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    let SessionRequest { command, key, reply } = request;
    let tool = tool(command);
    let (result, receipt) = match grants.answered.iter().find(|(answered, _)| *answered == key) {
        // The same idempotency identity never mutates twice.
        Some((_, cached)) => (cached.clone(), receipt(tool, "duplicate")),
        None => {
            let (result, receipt) = decide(command, tool, grants, controller);
            if grants.answered.len() == ANSWERED {
                grants.answered.remove(0);
            }
            grants.answered.push((key, result.clone()));
            (result, receipt)
        }
    };
    observe(Notice::SessionTool(receipt))?;
    let _ = reply.send(result);
    Ok(())
}

/// Between runs no session can be extended or ended.
/// # Errors
/// A failed receipt drops the reply, so the tool fails instead of succeeding.
pub(super) fn inactive(
    request: SessionRequest,
    observe: impl FnOnce(Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    let tool = tool(request.command);
    observe(Notice::SessionTool(receipt(tool, "session_inactive")))?;
    let _ = request.reply.send(failure("session_inactive"));
    Ok(())
}

fn decide(
    command: SessionCommand,
    tool: &'static str,
    grants: &mut Grants,
    controller: &mut Controller,
) -> (ToolResult, Receipt) {
    let refuse = |outcome| (failure(outcome), receipt(tool, outcome));
    let (Some(listen), Some(opened)) = (grants.listen, grants.opened) else {
        return refuse("session_inactive");
    };
    if controller.closing() {
        return refuse("session_closing");
    }
    match command {
        SessionCommand::Invalid { .. } => refuse("invalid_arguments"),
        SessionCommand::End => {
            let outcome = if grants.ending { "duplicate" } else { "applied" };
            grants.ending = true;
            let result = ToolResult {
                content: json!({"ending_after_reply": true}),
                is_error: false,
            };
            (result, receipt(tool, outcome))
        }
        SessionCommand::Extend(_) if grants.ending => refuse("session_closing"),
        SessionCommand::Extend(seconds) => {
            // During playback the base is the projected post-play deadline.
            let now = Instant::now();
            let current = controller.idle_deadline().or_else(|| now.checked_add(listen));
            let base = [current, grants.granted].into_iter().flatten().fold(now, Instant::max);
            let (deadline, capped) = grants.extend(base, seconds, opened);
            controller.extend_idle(deadline);
            let deadline_ms = u64::try_from(deadline.saturating_duration_since(opened).as_millis()).unwrap_or(u64::MAX);
            let result = ToolResult {
                content: json!({"deadline_ms": deadline_ms, "capped": capped}),
                is_error: false,
            };
            let mut receipt = receipt(tool, "applied");
            receipt.deadline_ms = Some(deadline_ms);
            receipt.capped = Some(capped);
            (result, receipt)
        }
    }
}

fn tool(command: SessionCommand) -> &'static str {
    match command {
        SessionCommand::End | SessionCommand::Invalid { end: true } => END,
        SessionCommand::Extend(_) | SessionCommand::Invalid { end: false } => EXTEND,
    }
}

fn failure(error: &str) -> ToolResult {
    ToolResult {
        content: json!({"error": error}),
        is_error: true,
    }
}

fn receipt(tool: &'static str, outcome: &'static str) -> Receipt {
    Receipt {
        tool,
        outcome,
        deadline_ms: None,
        capped: None,
    }
}

#[cfg(test)]
#[path = "tests/session_tools_tests.rs"]
mod tests;
