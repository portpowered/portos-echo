use crate::Error;
use echo_contracts::{ids::TaskId, task::TaskEvent};

pub(super) const UNAVAILABLE: &str = "Muse is unavailable. Please try again.";

pub(super) struct Session {
    pub terminal: bool,
    pub cancelled: bool,
    pub pending: bool,
}

impl Session {
    pub fn new() -> Self {
        Self {
            terminal: false,
            cancelled: false,
            pending: false,
        }
    }

    pub fn before_poll(&mut self, id: &TaskId) -> Option<TaskEvent> {
        if self.pending {
            // A dropped poll future may have consumed a persisted event. Never replay it.
            return Some(self.fail(id, UNAVAILABLE));
        }
        self.pending = true;
        None
    }

    pub fn event(&mut self, id: &TaskId, bytes: &[u8]) -> TaskEvent {
        let Ok(event) = serde_json::from_slice::<TaskEvent>(bytes) else {
            return self.fail(id, UNAVAILABLE);
        };
        let received = match &event {
            TaskEvent::Progress { task_id, .. }
            | TaskEvent::Completed { task_id, .. }
            | TaskEvent::Failed { task_id, .. } => task_id,
        };
        if received != id {
            return self.fail(id, UNAVAILABLE);
        }
        self.pending = false;
        self.terminal = !matches!(event, TaskEvent::Progress { .. });
        event
    }

    pub fn fail(&mut self, id: &TaskId, message: &str) -> TaskEvent {
        self.pending = false;
        self.terminal = true;
        TaskEvent::Failed {
            task_id: id.clone(),
            retryable: false,
            message: message.into(),
        }
    }
}

pub(super) fn admission(status: u16, bytes: &[u8]) -> Result<TaskId, Error> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| Error::Decode)?;
    let object = value.as_object().ok_or(Error::Decode)?;
    let id = object
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .ok_or(Error::Decode)?;
    if status != 201
        || object.len() != 1
        || id.is_empty()
        || id.len() > 256
        || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
        || matches!(id, "." | "..")
    {
        return Err(Error::Decode);
    }
    TaskId::new(id).map_err(|_| Error::Decode)
}

#[cfg(test)]
mod tests;
