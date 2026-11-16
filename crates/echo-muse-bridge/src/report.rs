use std::path::Path;

use echo_contracts::{ids::TaskId, task::TaskEvent};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::{Clock, Record, StoreError, TaskState, TaskStore};

#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("invalid report: {0}")]
    Invalid(#[from] serde_json::Error),
    #[error("unknown task_id")]
    Unknown,
    #[error("expired task_id")]
    Expired,
    #[error("cancelled task_id")]
    Cancelled,
    #[error("task_id already reported")]
    Reported,
    #[error("report persistence failed: {0}")]
    Store(#[from] StoreError),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    task_id: TaskId,
    status: Status,
    summary: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Completed,
    Failed,
}

/// Reports carry results only, never consent, authorization, or approval.
pub struct ReportIntake<'a, C, S> {
    pub(crate) clock: &'a C,
    pub(crate) store: &'a S,
    pub(crate) changed: &'a Notify,
}

impl<C: Clock, S: TaskStore> ReportIntake<'_, C, S> {
    /// Accept exactly one terminal report for a matching live task.
    /// # Errors
    /// Rejects malformed input, nonlive IDs, and unsuccessful persistence.
    pub fn accept(&self, stdin_json: &str) -> Result<Value, ReportError> {
        let report: Report = serde_json::from_str(stdin_json)?;
        let result = self.store.transaction(|snapshot| {
            let Some(record) = snapshot.records.iter_mut().find(|record| record.id == report.task_id) else {
                return Ok(Err(ReportError::Unknown));
            };
            record.expire(self.clock.now());
            let error = match record.state {
                TaskState::Live => None,
                TaskState::Expired => Some(ReportError::Expired),
                TaskState::Cancelled => Some(ReportError::Cancelled),
                TaskState::Completed | TaskState::Failed => Some(ReportError::Reported),
            };
            if let Some(error) = error {
                return Ok(Err(error));
            }
            record.state = match report.status {
                Status::Completed => TaskState::Completed,
                Status::Failed => TaskState::Failed,
            };
            record.events.push_back(match report.status {
                Status::Completed => TaskEvent::Completed {
                    task_id: report.task_id.clone(),
                    summary: report.summary,
                },
                Status::Failed => TaskEvent::Failed {
                    task_id: report.task_id.clone(),
                    retryable: false,
                    message: report.summary,
                },
            });
            Ok(Ok(json!({"accepted": true, "task_id": report.task_id})))
        })?;
        self.changed.notify_waiters();
        result
    }
}

impl Record {
    pub(crate) fn expire(&mut self, now: u64) {
        if self.state == TaskState::Live && now >= self.deadline {
            self.state = TaskState::Expired;
            self.events.push_back(TaskEvent::Failed {
                task_id: self.id.clone(),
                retryable: false,
                message: "Muse did not report a result in time".into(),
            });
        }
    }
}

/// SDK handler stdout, last stderr line, and exit code, without performing I/O.
#[must_use]
pub fn handler_output(result: Result<Value, ReportError>) -> (String, String, i32) {
    match result {
        Ok(value) => (value.to_string(), String::new(), 0),
        Err(error) => (String::new(), error.to_string().replace(['\r', '\n'], " "), 1),
    }
}

/// The only command offered by this crate. No command grants user consent.
#[must_use]
pub fn report_command_spec(exec: &Path) -> (String, Value) {
    (
        "echo.report.result.json".into(),
        json!({
        "description": "Report the result of a live Echo task. This is not user consent.",
            "exec": [exec],
            "required": {
            "task_id": {"type": "string", "description": "The live Echo task identifier."},
            "status": {"type": "string", "description": "completed or failed."},
            "summary": {"type": "string", "description": "A short result summary."}
            },
            "timeout_ms": 10000
        }),
    )
}
