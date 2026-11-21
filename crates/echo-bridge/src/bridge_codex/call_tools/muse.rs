//! Muse authority is bound to a native handoff, turn and live Wyoming run.
use crate::{
    Error,
    bridge_muse::{Access, Backend},
    bridge_runtime::bridge_muse_work,
    bridge_trace::Trace,
};
use echo_agent_api::{AgentBackend, Task};
use echo_codex::codex_protocol::{ToolContent, ToolResult};
use echo_contracts::ids::DelegationId;
use serde_json::Value;
use std::sync::Arc;
use tokio::{sync::oneshot, time::Instant};

pub(crate) struct MuseTool {
    backend: Arc<Backend>,
    trace: Trace,
    run: Option<Run>,
    access: Option<Access>,
}

struct Run {
    id: String,
    deadline: Instant,
    turn: Option<String>,
    handoff: Option<DelegationId>,
    pending: Option<DelegationId>,
}

impl MuseTool {
    pub(crate) fn new(backend: Arc<Backend>, trace: Trace, access: Option<Access>) -> Self {
        Self {
            backend,
            trace,
            run: None,
            access,
        }
    }

    pub(super) fn start_run(&mut self, deadline: Instant) -> Result<(), Error> {
        self.run = self.trace.current_run()?.map(|id| Run {
            id,
            deadline,
            turn: None,
            handoff: None,
            pending: None,
        });
        Ok(())
    }

    pub(super) fn finish_run(&mut self) {
        self.run = None;
    }

    pub(super) fn handoff(&mut self, id: DelegationId) {
        if let Some(run) = &mut self.run {
            if run.turn.is_some() && run.handoff.is_none() {
                run.handoff = Some(id);
            } else if run.pending.is_none() {
                run.pending = Some(id);
            }
        }
    }

    pub(super) fn start_turn(&mut self, turn: &str) {
        if let Some(run) = &mut self.run {
            run.turn = Some(turn.into());
            run.handoff = run.pending.take();
        }
    }

    pub(super) fn finish_turn(&mut self) {
        if let Some(run) = &mut self.run {
            run.turn = None;
            run.handoff = None;
        }
    }

    pub(super) fn invocation(&self, turn: &str) -> Option<Invocation<Backend>> {
        if self.access.as_ref().is_some_and(|access| !access.allows()) {
            return None;
        }
        let run = self.run.as_ref().filter(|run| run.turn.as_deref() == Some(turn))?;
        Some(Invocation {
            backend: self.backend.clone(),
            trace: self.trace.clone(),
            run: run.id.clone(),
            deadline: run.deadline,
            delegation: run.handoff.clone()?,
        })
    }
}

pub(super) struct Invocation<B> {
    backend: Arc<B>,
    trace: Trace,
    run: String,
    deadline: Instant,
    delegation: DelegationId,
}

pub(super) async fn invoke(
    invocation: Option<Invocation<impl AgentBackend>>,
    args: &Value,
    mut stop: oneshot::Receiver<()>,
) -> ToolResult {
    let Some(invocation) = invocation else {
        return reply("tool request refused", false);
    };
    let Some(request) = request(args) else {
        return reply("tool request refused", false);
    };
    let remaining = invocation.deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() || stop.try_recv() != Err(oneshot::error::TryRecvError::Empty) {
        return reply("tool request cancelled", false);
    }
    let task = Task {
        delegation_id: invocation.delegation,
        instructions: request.into(),
        context: Value::Null,
        deadline_seconds: remaining
            .as_secs()
            .saturating_add(u64::from(remaining.subsec_nanos() > 0)),
        // Required by the existing Muse wire. The service does not enforce this field.
        max_tool_calls: 8,
    };
    match bridge_muse_work::execute(&*invocation.backend, task, &invocation.run, invocation.trace, stop).await {
        Ok(Some(result)) => reply(&result.text, result.success()),
        Ok(None) => reply("tool request cancelled", false),
        Err(_) => reply(bridge_muse_work::UNAVAILABLE, false),
    }
}

fn request(args: &Value) -> Option<&str> {
    let object = args.as_object().filter(|object| object.len() == 1)?;
    let text = object.get("request")?.as_str()?.trim();
    (!text.is_empty()).then_some(text)
}

fn reply(text: &str, success: bool) -> ToolResult {
    ToolResult {
        content_items: vec![ToolContent::InputText { text: text.into() }],
        success,
    }
}

#[cfg(test)]
mod tests;
