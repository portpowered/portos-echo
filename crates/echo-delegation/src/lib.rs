//! Bridge-hosted client delegation to terminal commentary. No Live socket is owned here.
//!
//! Construct the MCP client from the selected configured server and token provider.
//! Reuse one runner to retain request accounting. Drive `run` to completion; if its
//! future is abandoned, drop the entire runner to stop the backend worker. A trace
//! observes preparation/dispatch attempts, not remote delivery or authorization.
use echo_agent_api::{AgentBackend, Task, ToolError, ToolHost, ToolResult, ToolSpec};
use echo_agent_loop::{InitError, ModelCredential, ResponsesBackend};
use echo_config::{AgentConfig, LiveMode};
use echo_contracts::{
    ids::DelegationId,
    live::{DelegationTarget, LiveEvent},
    task::TaskEvent,
};
use echo_mcp_client::{McpClient, TokenProvider};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TraceEvent {
    DelegationCreated { delegation_id: DelegationId },
    ToolCall { name: String },
    CommentaryAppend { delegation_id: DelegationId },
}
impl TraceEvent {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::DelegationCreated { .. } => "session.delegation.created",
            Self::ToolCall { .. } => "tools/call",
            Self::CommentaryAppend { .. } => "session.commentary.append",
        }
    }
}
/// Synchronous observation only; implementations must not block or expose secrets.
pub trait TraceSink: Send + Sync {
    fn record(&self, event: TraceEvent);
}

struct TracingHost<P, S> {
    host: Arc<McpClient<P>>,
    sink: Arc<S>,
}
impl<P: TokenProvider, S: TraceSink> ToolHost for TracingHost<P, S> {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>, ToolError> {
        self.host.list_tools().await
    }
    async fn call_tool(&self, name: &str, args: Value, key: &str) -> Result<ToolResult, ToolError> {
        self.sink.record(TraceEvent::ToolCall { name: name.into() });
        self.host.call_tool(name, args, key).await
    }
}

pub struct DelegationRunner<P, C, S> {
    backend: ResponsesBackend<TracingHost<P, S>, C>,
    config: AgentConfig,
    sink: Arc<S>,
}
impl<P: TokenProvider + 'static, C: ModelCredential + Send + Sync + 'static, S: TraceSink + 'static>
    DelegationRunner<P, C, S>
{
    /// No credential acquisition or connection occurs during construction.
    /// # Errors
    /// Returns the backend's model, client or loopback endpoint validation error.
    pub fn new(
        config: AgentConfig,
        mode: LiveMode,
        host: Arc<McpClient<P>>,
        credential: Arc<C>,
        sink: Arc<S>,
        model_base: Option<&str>,
    ) -> Result<Self, InitError> {
        let host = Arc::new(TracingHost {
            host,
            sink: sink.clone(),
        });
        let backend = match model_base {
            Some(base) => ResponsesBackend::loopback(config.clone(), mode, host, credential, base)?,
            None => ResponsesBackend::new(config.clone(), mode, host, credential)?,
        };
        Ok(Self { backend, config, sink })
    }
    /// Returns prepared commentary; the host owns sending it to Live.
    /// Drive to terminal, or drop this runner when abandoning the future.
    /// # Errors
    /// Refuses responses-target execution, failed submission or missing terminal.
    pub async fn run(&mut self, event: LiveEvent, instructions: String) -> Result<Option<LiveEvent>, DelegationError> {
        execute(&self.backend, &self.config, &*self.sink, event, instructions).await
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum DelegationError {
    #[error("unsupported delegation target")]
    UnsupportedTarget,
    #[error("delegation submission failed")]
    Submission,
    #[error("delegation missing terminal event")]
    MissingTerminal,
}

fn task(event: &LiveEvent, instructions: String, config: &AgentConfig) -> Result<Option<Task>, DelegationError> {
    let LiveEvent::DelegationCreated { delegation, .. } = event else {
        return Ok(None);
    };
    if delegation.target != DelegationTarget::Client {
        return Err(DelegationError::UnsupportedTarget);
    }
    Ok(Some(Task {
        delegation_id: delegation.id.clone(),
        instructions,
        context: serde_json::to_value(event).map_err(|_| DelegationError::Submission)?,
        deadline_seconds: config.task_deadline_seconds,
        max_tool_calls: config.max_tool_calls,
    }))
}

async fn execute(
    backend: &impl AgentBackend,
    config: &AgentConfig,
    sink: &impl TraceSink,
    event: LiveEvent,
    instructions: String,
) -> Result<Option<LiveEvent>, DelegationError> {
    let Some(task) = task(&event, instructions, config)? else {
        return Ok(None);
    };
    let delegation_id = task.delegation_id.clone();
    sink.record(TraceEvent::DelegationCreated {
        delegation_id: delegation_id.clone(),
    });
    let id = backend.submit(task).await.map_err(|_| DelegationError::Submission)?;
    while let Some(event) = backend.next_event(&id).await {
        let text = match event {
            TaskEvent::Completed { summary, .. } => summary,
            TaskEvent::Failed { message, .. } => message,
            TaskEvent::Progress { .. } => continue,
        };
        sink.record(TraceEvent::CommentaryAppend {
            delegation_id: delegation_id.clone(),
        });
        return Ok(Some(LiveEvent::CommentaryAppend {
            delegation_id,
            text,
            extra: std::collections::BTreeMap::default(),
        }));
    }
    Err(DelegationError::MissingTerminal)
}

#[cfg(test)]
mod delegation_tests;
