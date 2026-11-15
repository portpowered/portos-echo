//! Runtime-free delegated-task boundary for bridge-side consumers.
use core::future::Future;
use echo_contracts::ids::{DelegationId, TaskId};
use echo_contracts::task::TaskEvent;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub delegation_id: DelegationId,
    pub instructions: String,
    pub context: Value,
    pub deadline_seconds: u64,
    pub max_tool_calls: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AgentError {
    #[error("unknown task")]
    UnknownTask,
    #[error("unscripted task")]
    UnscriptedTask,
}

pub trait AgentBackend {
    fn submit(&self, task: Task) -> impl Future<Output = Result<TaskId, AgentError>> + Send + '_;
    fn next_event<'a>(&'a self, id: &'a TaskId) -> impl Future<Output = Option<TaskEvent>> + Send + 'a;
    fn cancel<'a>(&'a self, id: &'a TaskId) -> impl Future<Output = Result<(), AgentError>> + Send + 'a;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: Value,
    pub is_error: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ToolError {
    #[error("transport error")]
    Transport,
    #[error("unauthorized")]
    Unauthorized,
    #[error("denied")]
    Denied,
    #[error("timeout")]
    Timeout,
    #[error("server error: {0}")]
    Server(String),
}

pub trait ToolHost {
    fn list_tools(&self) -> impl Future<Output = Result<Vec<ToolSpec>, ToolError>> + Send + '_;
    fn call_tool<'a>(
        &'a self,
        name: &'a str,
        args: Value,
        idempotency_key: &'a str,
    ) -> impl Future<Output = Result<ToolResult, ToolError>> + Send + 'a;
}

#[cfg(any(test, feature = "fakes"))]
pub mod fakes;

#[cfg(test)]
mod tool_tests;

#[cfg(test)]
mod contract_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::{BackendCall, BackendScript, FakeBackend};
    use echo_contracts::ids::DelegationId;
    use pollster::block_on;
    use serde_json::json;

    fn task() -> Task {
        Task {
            delegation_id: DelegationId::new("d-1").unwrap(),
            instructions: "echo".into(),
            context: json!({"message": "hello"}),
            deadline_seconds: 60,
            max_tool_calls: 8,
        }
    }

    fn id(value: &str) -> TaskId {
        TaskId::new(value).unwrap()
    }

    fn script(value: &str) -> BackendScript {
        BackendScript {
            task: task(),
            id: id(value),
            events: vec![
                TaskEvent::Progress {
                    task_id: id(value),
                    message: "working".into(),
                },
                TaskEvent::Completed {
                    task_id: id(value),
                    summary: "done".into(),
                },
            ],
        }
    }

    #[test]
    fn backend_b1_records_complete_submission_and_ordered_events() {
        let expected = script("task-1");
        let backend = FakeBackend::new(vec![expected.clone()]);
        let task_id = block_on(backend.submit(task())).unwrap();
        assert_eq!(task_id, id("task-1"));
        for event in expected.events {
            assert_eq!(block_on(backend.next_event(&task_id)), Some(event));
        }
        assert_eq!(block_on(backend.next_event(&task_id)), None);
        assert_eq!(
            backend.calls(),
            vec![
                BackendCall::Submit(task()),
                BackendCall::NextEvent(task_id.clone()),
                BackendCall::NextEvent(task_id.clone()),
                BackendCall::NextEvent(task_id),
            ]
        );
    }

    #[test]
    fn backend_b2_returns_scripted_failure_without_success() {
        let failure = TaskEvent::Failed {
            task_id: id("task-err"),
            retryable: false,
            message: "stopped".into(),
        };
        let backend = FakeBackend::new(vec![BackendScript {
            events: vec![failure.clone()],
            ..script("task-err")
        }]);
        let task_id = block_on(backend.submit(task())).unwrap();
        assert_eq!(block_on(backend.next_event(&task_id)), Some(failure));
        assert_eq!(block_on(backend.next_event(&task_id)), None);
    }

    #[test]
    fn backend_b3_mismatches_preserve_scripts_and_record_attempts() {
        let mut variants = vec![task(); 5];
        variants[0].instructions = "different".into();
        variants[1].context = json!({"message": "different"});
        variants[2].deadline_seconds = 0;
        variants[3].max_tool_calls = 0;
        variants[4].delegation_id = DelegationId::new("d-2").unwrap();
        let backend = FakeBackend::new(vec![script("task-1"), script("task-2")]);
        for attempt in &variants {
            assert_eq!(
                block_on(backend.submit(attempt.clone())),
                Err(AgentError::UnscriptedTask)
            );
        }
        assert_eq!(
            backend.calls(),
            variants.into_iter().map(BackendCall::Submit).collect::<Vec<_>>()
        );
        assert_eq!(block_on(backend.submit(task())), Ok(id("task-1")));
        assert_eq!(block_on(backend.submit(task())), Ok(id("task-2")));
        assert_eq!(block_on(backend.submit(task())), Err(AgentError::UnscriptedTask));
    }

    #[test]
    fn backend_b4_interleaving_and_exhaustion_preserve_other_tasks() {
        let first = script("task-1");
        let mut second = script("task-2");
        second.task.instructions = "second".into();
        let backend = FakeBackend::new(vec![first.clone(), second.clone()]);
        // Matching a later script must leave the earlier submission available.
        assert_eq!(block_on(backend.submit(second.task.clone())), Ok(second.id.clone()));
        assert_eq!(block_on(backend.submit(first.task)), Ok(first.id.clone()));
        assert_eq!(block_on(backend.next_event(&first.id)), Some(first.events[0].clone()));
        assert_eq!(block_on(backend.next_event(&second.id)), Some(second.events[0].clone()));
        assert_eq!(block_on(backend.next_event(&first.id)), Some(first.events[1].clone()));
        assert_eq!(block_on(backend.next_event(&first.id)), None);
        assert_eq!(block_on(backend.next_event(&second.id)), Some(second.events[1].clone()));
        assert_eq!(block_on(backend.next_event(&second.id)), None);
    }

    #[test]
    fn backend_b5_cancel_is_idempotent_and_discards_only_target_events() {
        let backend = FakeBackend::new(vec![script("task-1"), script("task-2")]);
        let first = block_on(backend.submit(task())).unwrap();
        let second = block_on(backend.submit(task())).unwrap();
        assert_eq!(block_on(backend.cancel(&first)), Ok(()));
        assert_eq!(block_on(backend.cancel(&first)), Ok(()));
        assert_eq!(block_on(backend.next_event(&first)), None);
        assert_eq!(
            block_on(backend.next_event(&second)),
            Some(script("task-2").events.remove(0))
        );
        assert_eq!(
            &backend.calls()[2..4],
            &[BackendCall::Cancel(first.clone()), BackendCall::Cancel(first),]
        );
    }

    #[test]
    fn backend_b6_unknown_ids_and_instances_are_isolated() {
        let backend = FakeBackend::new(vec![script("task-1")]);
        let isolated = FakeBackend::default();
        let known = block_on(backend.submit(task())).unwrap();
        let unknown = id("missing");
        assert_eq!(block_on(backend.cancel(&unknown)), Err(AgentError::UnknownTask));
        assert_eq!(block_on(backend.next_event(&unknown)), None);
        assert_eq!(block_on(isolated.next_event(&known)), None);
        assert_eq!(block_on(isolated.submit(task())), Err(AgentError::UnscriptedTask));
        assert_eq!(
            block_on(backend.next_event(&known)),
            Some(script("task-1").events.remove(0))
        );
        assert_eq!(
            &backend.calls()[1..3],
            &[BackendCall::Cancel(unknown.clone()), BackendCall::NextEvent(unknown),]
        );
    }

    #[test]
    fn backend_b7_empty_script_remains_empty() {
        let backend = FakeBackend::new(vec![BackendScript {
            events: vec![],
            ..script("empty")
        }]);
        let task_id = block_on(backend.submit(task())).unwrap();
        assert_eq!(block_on(backend.next_event(&task_id)), None);
        assert_eq!(block_on(backend.next_event(&task_id)), None);
    }
}
