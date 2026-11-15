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

#[cfg(any(test, feature = "fakes"))]
pub mod fakes;
