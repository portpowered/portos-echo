//! One admitted task and terminal; the run owner controls cancellation.
use super::bridge_resolution::Outcome;
use crate::{Error, bridge_trace::Trace};
use echo_agent_api::{AgentBackend, Task};
use echo_contracts::{ids::TaskId, task::TaskEvent};
use serde_json::json;
use tokio::sync::oneshot;

pub(crate) struct Prepared {
    pub text: String,
    pub(super) outcome: Outcome,
}

impl Prepared {
    pub(super) fn new(text: &str, outcome: Outcome) -> Self {
        Self {
            text: text.into(),
            outcome,
        }
    }
}

pub(crate) const UNAVAILABLE: &str = "Muse is unavailable. Please try again.";
impl Prepared {
    pub(crate) fn success(&self) -> bool {
        matches!(self.outcome, Outcome::Completed)
    }
}

const DEADLINE: &str = "Muse did not report a result in time";

pub(crate) async fn execute(
    backend: &impl AgentBackend,
    task: Task,
    run: &str,
    mut trace: Trace,
    mut stop: oneshot::Receiver<()>,
) -> Result<Option<Prepared>, Error> {
    let delegation = task.delegation_id.clone();
    // Admission is a bounded single mutation. Do not drop an uncertain request on
    // cancellation: retain its admitted ID if possible and then cancel it.
    let Ok(id) = backend.submit(task).await else {
        return Ok(if stop.try_recv() == Err(oneshot::error::TryRecvError::Empty) {
            Some(Prepared::new(UNAVAILABLE, Outcome::Failed("submission_failed")))
        } else {
            None
        });
    };
    let result = async {
        trace.record(
            "muse.submit",
            Some(run),
            json!({"delegation_id":delegation,"task_id":id}),
        )?;
        loop {
            let event = tokio::select! {
                biased;
                _ = &mut stop => return Ok(None),
                event = backend.next_event(&id) => event.unwrap_or_else(|| TaskEvent::Failed {
                    task_id: id.clone(), retryable: false, message: UNAVAILABLE.into()
                }),
            };
            if let Some(prepared) = observe(event, &id, &delegation, run, &mut trace)? {
                return Ok(Some(prepared));
            }
        }
    }
    .await;
    // Only abandoned work is cancelled remotely. Cancelling a consumed terminal
    // would overwrite its completed/expired state and send an extra gadget message.
    if !matches!(&result, Ok(Some(_))) {
        let _ = backend.cancel(&id).await;
    }
    result
}

fn observe(
    event: TaskEvent,
    id: &TaskId,
    delegation: &echo_contracts::ids::DelegationId,
    run: &str,
    trace: &mut Trace,
) -> Result<Option<Prepared>, Error> {
    let mut fields = json!({"delegation_id":delegation,"task_id":id});
    let (received, text, outcome) = match event {
        TaskEvent::Progress { task_id, .. } => {
            if task_id != *id {
                return Err(Error::Decode);
            }
            trace.record("muse.progress", Some(run), fields)?;
            return Ok(None);
        }
        TaskEvent::Completed { task_id, summary } => {
            fields["status"] = "completed".into();
            (task_id, summary, Outcome::Completed)
        }
        TaskEvent::Failed { task_id, message, .. } => {
            fields["status"] = "failed".into();
            fields["retryable"] = false.into();
            // Failures use only safe authored messages at the speech boundary.
            let message = match message.as_str() {
                DEADLINE | "Muse test job failed" => message,
                _ => UNAVAILABLE.into(),
            };
            let outcome = if message == DEADLINE {
                Outcome::TimedOut
            } else {
                Outcome::Failed("task_failed")
            };
            (task_id, message, outcome)
        }
    };
    if received != *id {
        return Err(Error::Decode);
    }
    trace.record("muse.terminal", Some(run), fields)?;
    Ok(Some(Prepared { text, outcome }))
}

#[cfg(test)]
mod tests;
