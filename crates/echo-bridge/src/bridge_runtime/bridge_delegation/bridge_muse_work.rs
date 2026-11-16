//! One admitted task, original deadline and terminal; no worker owns a Live writer.
use crate::{Error, bridge_trace::Trace};
use echo_agent_api::{AgentBackend, Task};
use echo_contracts::{ids::TaskId, task::TaskEvent};
use serde_json::json;
use std::time::Duration;
use tokio::{sync::oneshot, time::Instant};

const UNAVAILABLE: &str = "Muse is unavailable. Please try again.";
const DEADLINE: &str = "Muse did not report a result in time";

pub(super) async fn execute(
    backend: &impl AgentBackend,
    task: Task,
    run: &str,
    mut trace: Trace,
    mut stop: oneshot::Receiver<()>,
) -> Result<Option<String>, Error> {
    let deadline = Instant::now() + Duration::from_secs(task.deadline_seconds);
    let delegation = task.delegation_id.clone();
    // Admission is a bounded single mutation. Do not drop an uncertain request on
    // cancellation: retain its admitted ID if possible and then cancel it.
    let Ok(id) = backend.submit(task).await else {
        return Ok(if stop.try_recv() == Err(oneshot::error::TryRecvError::Empty) {
            Some(UNAVAILABLE.into())
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
                () = tokio::time::sleep_until(deadline) => TaskEvent::Failed {
                    task_id: id.clone(), retryable: false, message: DEADLINE.into()
                },
                event = backend.next_event(&id) => event.unwrap_or_else(|| TaskEvent::Failed {
                    task_id: id.clone(), retryable: false, message: UNAVAILABLE.into()
                }),
            };
            if let Some(text) = observe(event, &id, &delegation, run, &mut trace)? {
                return Ok(Some(text));
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
) -> Result<Option<String>, Error> {
    let mut fields = json!({"delegation_id":delegation,"task_id":id});
    let (received, text) = match event {
        TaskEvent::Progress { task_id, .. } => {
            if task_id != *id {
                return Err(Error::Decode);
            }
            trace.record("muse.progress", Some(run), fields)?;
            return Ok(None);
        }
        TaskEvent::Completed { task_id, summary } => {
            fields["status"] = "completed".into();
            (task_id, summary)
        }
        TaskEvent::Failed { task_id, message, .. } => {
            fields["status"] = "failed".into();
            fields["retryable"] = false.into();
            // Failures use only safe authored messages at the speech boundary.
            let message = match message.as_str() {
                DEADLINE | "Muse test job failed" => message,
                _ => UNAVAILABLE.into(),
            };
            (task_id, message)
        }
    };
    if received != *id {
        return Err(Error::Decode);
    }
    trace.record("muse.terminal", Some(run), fields)?;
    Ok(Some(text))
}

#[cfg(test)]
mod tests;
