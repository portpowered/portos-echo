//! Isolated, in-memory scripts. Limits are recorded, never enforced here.
use std::collections::{HashMap, VecDeque};
use std::future::{Future, ready};
use std::sync::{Mutex, MutexGuard};

use echo_contracts::ids::TaskId;
use echo_contracts::task::TaskEvent;

use crate::{AgentBackend, AgentError, Task, ToolError, ToolHost, ToolResult, ToolSpec};
use serde_json::Value;

/// One expected submission and the events to return for its assigned id.
#[derive(Clone, Debug)]
pub struct BackendScript {
    pub task: Task,
    pub id: TaskId,
    pub events: Vec<TaskEvent>,
}

/// Attempts in invocation order, including unsuccessful operations.
#[derive(Clone, Debug, PartialEq)]
pub enum BackendCall {
    Submit(Task),
    NextEvent(TaskId),
    Cancel(TaskId),
}

#[derive(Default)]
struct BackendState {
    scripts: Vec<BackendScript>,
    events: HashMap<TaskId, VecDeque<TaskEvent>>,
    calls: Vec<BackendCall>,
}

/// Matches the complete input and consumes only its first matching script.
#[derive(Default)]
pub struct FakeBackend {
    state: Mutex<BackendState>,
}

impl FakeBackend {
    /// Scripts should use distinct task ids and events correlated to those ids.
    #[must_use]
    pub fn new(scripts: Vec<BackendScript>) -> Self {
        Self {
            state: Mutex::new(BackendState {
                scripts,
                ..BackendState::default()
            }),
        }
    }

    /// Snapshot every attempt without sharing mutable state with the caller.
    #[must_use]
    pub fn calls(&self) -> Vec<BackendCall> {
        self.state().calls.clone()
    }

    fn state(&self) -> MutexGuard<'_, BackendState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl AgentBackend for FakeBackend {
    fn submit(&self, task: Task) -> impl Future<Output = Result<TaskId, AgentError>> + Send {
        let mut state = self.state();
        state.calls.push(BackendCall::Submit(task.clone()));
        let index = state.scripts.iter().position(|script| script.task == task);
        let result = index.map_or(Err(AgentError::UnscriptedTask), |index| {
            let script = state.scripts.remove(index);
            state.events.insert(script.id.clone(), script.events.into());
            Ok(script.id)
        });
        ready(result)
    }

    fn next_event(&self, id: &TaskId) -> impl Future<Output = Option<TaskEvent>> + Send {
        let mut state = self.state();
        state.calls.push(BackendCall::NextEvent(id.clone()));
        ready(state.events.get_mut(id).and_then(VecDeque::pop_front))
    }

    fn cancel(&self, id: &TaskId) -> impl Future<Output = Result<(), AgentError>> + Send {
        let mut state = self.state();
        state.calls.push(BackendCall::Cancel(id.clone()));
        ready(
            state
                .events
                .get_mut(id)
                .ok_or(AgentError::UnknownTask)
                .map(VecDeque::clear),
        )
    }
}

/// Exact input triple, also used to snapshot unsuccessful call attempts.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub name: String,
    pub args: Value,
    pub idempotency_key: String,
}

#[derive(Clone, Debug)]
pub struct ToolScript {
    pub call: ToolCall,
    pub result: Result<ToolResult, ToolError>,
}

struct ToolState {
    scripts: Vec<ToolScript>,
    calls: Vec<ToolCall>,
}

/// No transport, retries, authorization policy or idempotency ledger.
pub struct FakeToolHost {
    catalog: Result<Vec<ToolSpec>, ToolError>,
    state: Mutex<ToolState>,
}

impl FakeToolHost {
    #[must_use]
    pub fn new(catalog: Result<Vec<ToolSpec>, ToolError>, scripts: Vec<ToolScript>) -> Self {
        Self {
            catalog,
            state: Mutex::new(ToolState { scripts, calls: vec![] }),
        }
    }

    /// Snapshot every call attempt in invocation order.
    #[must_use]
    pub fn calls(&self) -> Vec<ToolCall> {
        self.state().calls.clone()
    }

    fn state(&self) -> MutexGuard<'_, ToolState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl ToolHost for FakeToolHost {
    fn list_tools(&self) -> impl Future<Output = Result<Vec<ToolSpec>, ToolError>> + Send {
        ready(self.catalog.clone())
    }

    fn call_tool(
        &self,
        name: &str,
        args: Value,
        idempotency_key: &str,
    ) -> impl Future<Output = Result<ToolResult, ToolError>> + Send {
        let call = ToolCall {
            name: name.into(),
            args,
            idempotency_key: idempotency_key.into(),
        };
        let mut state = self.state();
        state.calls.push(call.clone());
        let index = state.scripts.iter().position(|script| script.call == call);
        ready(index.map_or_else(
            || Err(ToolError::Server("unscripted tool call".into())),
            |index| state.scripts.remove(index).result,
        ))
    }
}
