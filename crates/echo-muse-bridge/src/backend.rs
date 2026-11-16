use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use echo_agent_api::{AgentBackend, AgentError, Task};
use echo_contracts::{ids::TaskId, task::TaskEvent};
use tokio::sync::Notify;

use crate::{Clock, Gadget, Record, ReportIntake, SideChat, StoreError, TaskState, TaskStore};

/// The shared store and notifier serialize result intake with event consumption.
pub struct MuseBackend<G, C, S> {
    gadget: G,
    clock: C,
    store: S,
    changed: Notify,
    sends: Mutex<HashMap<TaskId, Arc<tokio::sync::Mutex<()>>>>,
}

impl<G: Gadget, C: Clock, S: TaskStore> MuseBackend<G, C, S> {
    #[must_use]
    pub fn new(gadget: G, clock: C, store: S) -> Self {
        Self {
            gadget,
            clock,
            store,
            changed: Notify::new(),
            sends: Mutex::new(HashMap::new()),
        }
    }

    /// Intake acknowledges committed results only. It never grants user consent.
    pub fn report_intake(&self) -> ReportIntake<'_, C, S> {
        ReportIntake {
            clock: &self.clock,
            store: &self.store,
            changed: &self.changed,
        }
    }

    fn send_lock(&self, id: &TaskId) -> Arc<tokio::sync::Mutex<()>> {
        self.sends
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(id.clone())
            .or_default()
            .clone()
    }

    fn admit(&self, task: Task) -> Result<Record, StoreError> {
        let session = SideChat::fresh().map_err(|_| StoreError::Invalid)?;
        let id = TaskId::new(format!("task-{}", SideChat::fresh().map_err(|_| StoreError::Invalid)?))
            .map_err(|_| StoreError::Invalid)?;
        let deadline = self
            .clock
            .now()
            .checked_add(task.deadline_seconds)
            .ok_or(StoreError::Invalid)?;
        let record = Record {
            id,
            task,
            session,
            deadline,
            state: TaskState::Live,
            events: VecDeque::new(),
        };
        self.store.transaction(|snapshot| {
            snapshot.records.push(record.clone());
            Ok(())
        })?;
        Ok(record)
    }

    fn finish_send(&self, id: &TaskId, result: Result<crate::Reply, crate::GadgetError>) -> Result<(), StoreError> {
        self.store.transaction(|snapshot| {
            let record = snapshot
                .records
                .iter_mut()
                .find(|record| &record.id == id)
                .ok_or(StoreError::Invalid)?;
            record.expire(self.clock.now());
            if record.state == TaskState::Live {
                let event = if let Ok(reply) = result {
                    TaskEvent::Progress {
                        task_id: id.clone(),
                        message: reply.text,
                    }
                } else {
                    record.state = TaskState::Failed;
                    TaskEvent::Failed {
                        task_id: id.clone(),
                        retryable: false,
                        message: "Muse gadget request failed".into(),
                    }
                };
                record.events.push_back(event);
            }
            Ok(())
        })
    }
}

impl<G: Gadget, C: Clock, S: TaskStore> AgentBackend for MuseBackend<G, C, S> {
    async fn submit(&self, task: Task) -> Result<TaskId, AgentError> {
        let record = self.admit(task).map_err(|_| AgentError::UnscriptedTask)?;
        let gate = self.send_lock(&record.id);
        let _sending = gate.lock().await;
        // Cancellation can win after admission but before the first send.
        let live = self
            .store
            .transaction(|snapshot| {
                Ok(snapshot
                    .records
                    .iter()
                    .any(|item| item.id == record.id && item.state == TaskState::Live))
            })
            .map_err(|_| AgentError::UnscriptedTask)?;
        if live {
            let result = self
                .gadget
                .send_user_msg(record.session, &compose_task_message(&record.task, &record.id), 1)
                .await;
            self.finish_send(&record.id, result)
                .map_err(|_| AgentError::UnscriptedTask)?;
        }
        self.changed.notify_waiters();
        Ok(record.id)
    }

    async fn next_event<'a>(&'a self, id: &'a TaskId) -> Option<TaskEvent> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let (event, deadline) = self
                .store
                .transaction(|snapshot| {
                    let Some(record) = snapshot.records.iter_mut().find(|record| &record.id == id) else {
                        return Ok((None, None));
                    };
                    record.expire(self.clock.now());
                    Ok((
                        record.events.pop_front(),
                        (record.state == TaskState::Live).then_some(record.deadline),
                    ))
                })
                .ok()?;
            if event.is_some() || deadline.is_none() {
                return event;
            }
            tokio::select! {
                () = &mut changed => {},
                () = self.clock.wait_until(deadline?) => {},
            }
        }
    }

    async fn cancel<'a>(&'a self, id: &'a TaskId) -> Result<(), AgentError> {
        let session = self
            .store
            .transaction(|snapshot| {
                let Some(record) = snapshot.records.iter_mut().find(|record| &record.id == id) else {
                    return Ok(None);
                };
                record.state = TaskState::Cancelled;
                record.events.clear();
                Ok(Some(record.session))
            })
            .map_err(|_| AgentError::UnscriptedTask)?
            .ok_or(AgentError::UnknownTask)?;
        self.changed.notify_waiters();
        let gate = self.send_lock(id);
        let _sending = gate.lock().await;
        let _advisory = self
            .gadget
            .send_user_msg(
                session,
                &format!("Cancel task {id}. Do not call echo.report.result for it."),
                1,
            )
            .await;
        Ok(())
    }
}

/// Preserve task data verbatim; conversational replies never terminate a task.
#[must_use]
pub fn compose_task_message(task: &Task, id: &TaskId) -> String {
    format!(
        "{}\nContext: {}\nTask ID: {id}\nWhen you are finished, call the echo.report.result command with task_id {id}, status completed or failed, and a short summary. Do not report success any other way.",
        task.instructions, task.context
    )
}
