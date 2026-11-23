//! Run-scoped answers to bridge-owned session tools. Only this run owner
//! mutates waiting or end state; every settled invocation writes one receipt
//! before its result is released, so a failed receipt never reports success.
use super::{Error, Notice, session_controller::Controller};
use crate::bridge_codex::{SessionCommand, SessionRequest};
use echo_agent_api::ToolResult;
use echo_core_api::{SessionToolAction, SessionToolDecision, SessionToolRefusal};
use serde_json::json;

pub(super) const EXTEND: &str = "extend_session";
pub(super) const END: &str = "end_session";

/// Payload-free projection of one settled session tool invocation.
pub struct Receipt {
    pub tool: &'static str,
    pub outcome: &'static str,
    pub deadline_ms: Option<u64>,
    pub capped: Option<bool>,
}

/// Answer one admitted request for the active run.
/// # Errors
/// A failed receipt drops the reply, so the tool fails instead of succeeding.
pub(super) fn answer(
    request: SessionRequest,
    controller: &Controller,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    let SessionRequest {
        command,
        key,
        run,
        reply,
    } = request;
    let tool = tool(command);
    // Refusal precedes both controller mutation and idempotency-cache writes.
    let refused = if reply.is_closed() {
        Some("request_cancelled")
    } else if run
        .as_ref()
        .is_none_or(|run| !std::sync::Arc::ptr_eq(run, controller.token()))
    {
        Some("session_stale")
    } else {
        None
    };
    if let Some(outcome) = refused {
        observe(Notice::SessionTool(receipt(tool, outcome)))?;
        let _ = reply.send(failure(outcome));
        return Ok(());
    }
    let action = match command {
        SessionCommand::Extend(seconds) => SessionToolAction::Extend { seconds },
        SessionCommand::End => SessionToolAction::End,
        SessionCommand::Invalid { .. } => SessionToolAction::Invalid,
    };
    let (decision, cached) = controller.answer_tool(key, action)?;
    let (result, mut receipt) = project(tool, decision);
    if cached {
        receipt = self::receipt(tool, "duplicate");
    }
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

fn project(tool: &'static str, decision: SessionToolDecision) -> (ToolResult, Receipt) {
    match decision {
        SessionToolDecision::Refused { reason, .. } => {
            let outcome = match reason {
                SessionToolRefusal::InvalidArguments => "invalid_arguments",
                SessionToolRefusal::Inactive => "session_inactive",
                SessionToolRefusal::Closing => "session_closing",
            };
            (failure(outcome), receipt(tool, outcome))
        }
        SessionToolDecision::Ending { duplicate, .. } => {
            let outcome = if duplicate { "duplicate" } else { "applied" };
            let result = ToolResult {
                content: json!({"ending_after_reply": true}),
                is_error: false,
            };
            (result, receipt(tool, outcome))
        }
        SessionToolDecision::Extended {
            deadline_ms, capped, ..
        } => {
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
