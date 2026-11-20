//! Observation at the retained runner boundary; no executor or delivery authority.
use super::bridge_resolution::{Outcome, Resolution};
use crate::{Error, bridge_trace::Trace};
use echo_agent_api::{ToolError, ToolResult};
use echo_contracts::task::TaskEvent;
use serde_json::{Value, json};
use std::sync::Mutex;

#[derive(Default)]
pub(super) struct ClientTrace(Mutex<Option<State>>);

struct State {
    lifetime: Resolution,
    server: String,
    outcome: Outcome,
    failed: bool,
}

impl ClientTrace {
    pub fn begin(&self, trace: &Trace, run: &str, id: &str, task: &Value, server: &str) -> Result<(), Error> {
        *self.0.lock().map_err(|_| Error::Trace)? = Some(State {
            lifetime: Resolution::created(trace, run, id, task)?,
            server: server.into(),
            outcome: Outcome::Cancelled,
            failed: false,
        });
        Ok(())
    }

    pub fn terminal(&self, event: &TaskEvent) {
        if let Ok(mut state) = self.0.lock()
            && let Some(state) = state.as_mut()
        {
            state.outcome = match event {
                TaskEvent::Completed { .. } => Outcome::Completed,
                // This is the runner's authored failure category, never final
                // agent text. Completed text with these words stays completed.
                TaskEvent::Failed { message, .. } if message == "task timed out" => Outcome::TimedOut,
                TaskEvent::Failed { .. } => Outcome::Failed("task_failed"),
                TaskEvent::Progress { .. } => return,
            };
            state.lifetime.observed(state.outcome);
        }
    }

    pub fn tool(
        &self,
        name: &str,
        args: &Value,
        result: &Result<ToolResult, ToolError>,
        duration: std::time::Duration,
    ) {
        if let Ok(mut state) = self.0.lock()
            && let Some(state) = state.as_mut()
        {
            let (status, response) = match result {
                Ok(result) => (
                    if result.is_error { "failed" } else { "completed" },
                    json!({"content":result.content,"is_error":result.is_error}),
                ),
                Err(error) => (
                    if *error == ToolError::Denied {
                        "declined"
                    } else {
                        "failed"
                    },
                    json!({"error":error.to_string()}),
                ),
            };
            state.failed |= state
                .lifetime
                .tool(json!({
                    "kind":"mcp","name":name,"server":state.server,"status":status,
                    "duration_ms":u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
                    "request":args,"response":response
                }))
                .is_err();
        }
    }

    pub fn check(&self) -> Result<(), Error> {
        if self
            .0
            .lock()
            .map_err(|_| Error::Trace)?
            .as_ref()
            .is_some_and(|state| state.failed)
        {
            return Err(Error::Trace);
        }
        Ok(())
    }

    pub fn finish(
        &self,
        failure: Option<&'static str>,
        handback: &'static str,
        answer: Option<&str>,
    ) -> Result<(), Error> {
        let Some(mut state) = self.0.lock().map_err(|_| Error::Trace)?.take() else {
            return Ok(());
        };
        let outcome = match (state.outcome, failure) {
            (Outcome::Completed | Outcome::Cancelled, Some(reason)) => Outcome::Failed(reason),
            (outcome, _) => outcome,
        };
        state.lifetime.finish(outcome, handback, answer)
    }
}
