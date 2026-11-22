mod store;

#[cfg(test)]
mod tests;

use std::{collections::VecDeque, path::Path, sync::Mutex};

use echo_agent_api::Task;
use echo_contracts::{ids::TaskId, task::TaskEvent};
use serde::Deserialize;
use tokio::sync::watch;

use crate::{ServiceError, clock::Clock, link::Random};

use self::store::{Record, Snapshot, State, Store};

const TIMEOUT: &str = "Muse did not report a result in time";
const MAX_PROMPT: usize = 1024 * 1024;

pub(crate) struct Tasks {
    storage: Mutex<(Store, Snapshot)>,
    changed: watch::Sender<()>,
    pub(crate) clock: Box<dyn Clock>,
    pub(crate) random: Box<dyn Random>,
}

pub(crate) struct Submission {
    pub(crate) id: TaskId,
    pub(crate) session: String,
    pub(crate) deadline: u64,
    pub(crate) message: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Report {
    pub(crate) task_id: TaskId,
    status: ReportStatus,
    summary: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReportStatus {
    Completed,
    Failed,
}

impl Tasks {
    pub(crate) fn open(path: &Path, clock: impl Clock, random: impl Random) -> Result<Self, ServiceError> {
        Ok(Self {
            storage: Mutex::new(Store::open(path)?),
            changed: watch::channel(()).0,
            clock: Box::new(clock),
            random: Box::new(random),
        })
    }

    pub(crate) fn prepare(&self, task: &Task) -> Result<Submission, ServiceError> {
        let deadline = self
            .clock
            .now()
            .checked_add(task.deadline_seconds)
            .filter(|_| task.deadline_seconds != 0)
            .ok_or(ServiceError::InvalidDeadline)?;
        let id = TaskId::new(format!("task-{}", self.uuid()?)).map_err(|_| ServiceError::Storage)?;
        let session = self.uuid()?;
        let message = format!(
            "{}\nContext: {}\nTask ID: {id}\nWhen you are finished, call the echo.report.result command with task_id {id}, status completed or failed, and a short summary. Do not report success any other way.",
            task.instructions, task.context
        );
        if message.len() > MAX_PROMPT {
            return Err(ServiceError::PeerUnavailable);
        }
        Ok(Submission {
            id,
            session,
            deadline,
            message,
        })
    }

    pub(crate) fn admit(&self, task: &Submission) -> Result<(), ServiceError> {
        let mut storage = self.storage.lock().map_err(|_| ServiceError::Storage)?;
        let (store, snapshot) = &mut *storage;
        if self.clock.now() >= task.deadline {
            return Err(ServiceError::InvalidDeadline);
        }
        if snapshot
            .tasks
            .iter()
            .any(|record| record.task_id == task.id || record.session_id == task.session)
        {
            return Err(ServiceError::Storage);
        }
        let mut pending = snapshot.clone();
        pending.tasks.push(Record {
            task_id: task.id.clone(),
            session_id: task.session.clone(),
            deadline_unix_seconds: task.deadline,
            state: State::Live,
            events: VecDeque::new(),
        });
        store.save(&pending)?;
        *snapshot = pending;
        Ok(())
    }

    pub(crate) fn uuid(&self) -> Result<String, ServiceError> {
        let mut bytes = [0; 16];
        self.random.fill(&mut bytes)?;
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let hex = hex::encode(bytes);
        Ok(format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        ))
    }

    pub(crate) fn report(&self, report: Report) -> Result<(), ServiceError> {
        self.edit(&report.task_id, |record| {
            if !record.live_at(self.clock.now()) {
                return Err(ServiceError::UnknownTask);
            }
            let task_id = record.task_id.clone();
            let (state, event) = match report.status {
                ReportStatus::Completed => (
                    State::Completed,
                    TaskEvent::Completed {
                        task_id,
                        summary: report.summary,
                    },
                ),
                ReportStatus::Failed => (
                    State::Failed,
                    TaskEvent::Failed {
                        task_id,
                        message: report.summary,
                        retryable: false,
                    },
                ),
            };
            record.state = state;
            record.events.push_back(event);
            Ok(())
        })?
    }

    pub(crate) fn progress(&self, id: &TaskId, message: String) -> Result<bool, ServiceError> {
        self.edit(id, |record| {
            if !record.live_at(self.clock.now()) {
                return false;
            }
            record.events.push_back(TaskEvent::Progress {
                task_id: id.clone(),
                message,
            });
            true
        })
    }

    pub(crate) fn live(&self, id: &TaskId) -> Result<bool, ServiceError> {
        self.edit(id, |record| record.live_at(self.clock.now()))
    }

    pub(crate) fn cancel(&self, id: &TaskId) -> Result<Option<String>, ServiceError> {
        self.edit(id, |record| {
            record.live_at(self.clock.now()).then(|| {
                record.cancel();
                record.session_id.clone()
            })
        })
    }

    // A definite refusal abandons the admission unless a genuine terminal event
    // already won the race. In that case the caller still gets the task ID.
    pub(crate) fn refuse(&self, id: &TaskId) -> Result<bool, ServiceError> {
        self.edit(id, |record| {
            if record.live_at(self.clock.now()) {
                record.cancel();
            }
            matches!(record.state, State::Completed | State::Failed | State::Expired)
        })
    }

    pub(crate) async fn next_event(&self, id: &TaskId) -> Result<Option<TaskEvent>, ServiceError> {
        let mut changes = self.changed.subscribe();
        loop {
            let (event, deadline) = self.edit(id, |record| {
                let live = record.live_at(self.clock.now());
                (record.events.pop_front(), live.then_some(record.deadline_unix_seconds))
            })?;
            let Some(deadline) = deadline.filter(|_| event.is_none()) else {
                return Ok(event);
            };
            tokio::select! {
                _ = changes.changed() => {}
                () = self.clock.wait_until(deadline) => {}
            }
        }
    }

    fn edit<T>(&self, id: &TaskId, edit: impl FnOnce(&mut Record) -> T) -> Result<T, ServiceError> {
        let mut storage = self.storage.lock().map_err(|_| ServiceError::Storage)?;
        let (store, snapshot) = &mut *storage;
        store.ready()?;
        let index = snapshot
            .tasks
            .iter()
            .position(|record| &record.task_id == id)
            .ok_or(ServiceError::UnknownTask)?;
        let mut record = snapshot.tasks[index].clone();
        let result = edit(&mut record);
        if record != snapshot.tasks[index] {
            let mut pending = snapshot.clone();
            pending.tasks[index] = record;
            store.save(&pending)?;
            *snapshot = pending;
            self.changed.send_replace(());
        }
        Ok(result)
    }
}

impl Record {
    fn live_at(&mut self, now: u64) -> bool {
        if self.state == State::Live && now >= self.deadline_unix_seconds {
            self.state = State::Expired;
            self.events.push_back(TaskEvent::Failed {
                task_id: self.task_id.clone(),
                message: TIMEOUT.into(),
                retryable: false,
            });
        }
        self.state == State::Live
    }

    fn cancel(&mut self) {
        self.state = State::Cancelled;
        self.events.clear();
    }
}
