//! Bridge-hosted client delegation to terminal commentary. No Live socket is owned here.
//!
//! Construct the MCP client from the selected configured server and token provider.
//! Reuse one runner to retain request accounting. Drive `run` to completion; if its
//! future is abandoned, drop the entire runner to stop the backend worker. A trace
//! observes preparation/dispatch attempts, not remote delivery or authorization.
pub mod codex_dispatch;
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
    /// Observe an actual tool response without changing its backend value.
    /// Implementations must not block or log tool content or credentials.
    fn tool_result(&self, _name: &str, _content: &Value, _is_error: bool) {}
    /// Observe the actual terminal event before unchanged commentary preparation.
    fn task_terminal(&self, _event: &TaskEvent) {}
    /// Observe one completed invocation, including transport failures. Hosts must
    /// redact and bound payloads before persisting; this never changes the result.
    fn tool_completed(
        &self,
        _name: &str,
        _arguments: &Value,
        _result: &Result<ToolResult, ToolError>,
        _duration: std::time::Duration,
    ) {
    }
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
        let started = tokio::time::Instant::now();
        let result = self.host.call_tool(name, args.clone(), key).await;
        self.sink.tool_completed(name, &args, &result, started.elapsed());
        if let Ok(result) = &result {
            self.sink.tool_result(name, &result.content, result.is_error);
        }
        result
    }
}

pub struct DelegationRunner<P, C, S> {
    backend: ResponsesBackend<TracingHost<P, S>, C>,
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
            Some(base) => ResponsesBackend::loopback(config, mode, host, credential, base)?,
            None => ResponsesBackend::new(config, mode, host, credential)?,
        };
        Ok(Self { backend, sink })
    }
    /// Returns prepared commentary; the host owns sending it to Live.
    /// Drive to terminal, or drop this runner when abandoning the future.
    /// # Errors
    /// Refuses responses-target execution, failed submission or missing terminal.
    pub async fn run(&mut self, event: LiveEvent, instructions: String) -> Result<Option<LiveEvent>, DelegationError> {
        self.run_until_cancelled(event, instructions, std::future::pending())
            .await
    }

    /// Drive a bridge-owned task until it completes or the run cancels it.
    /// Cancellation joins the backend worker before returning.
    /// # Errors
    /// Refuses unsupported targets, failed submission or missing terminal events.
    pub async fn run_until_cancelled(
        &mut self,
        event: LiveEvent,
        instructions: String,
        cancelled: impl std::future::Future<Output = ()> + Send,
    ) -> Result<Option<LiveEvent>, DelegationError> {
        execute(&self.backend, &*self.sink, event, instructions, cancelled, |task| {
            std::future::ready(self.backend.submit_for_run(task))
        })
        .await
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

fn task(event: &LiveEvent, instructions: String) -> Result<Option<Task>, DelegationError> {
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
        // Required by Task; submit_for_run uses the owner's lifetime instead.
        deadline_seconds: 0,
        max_tool_calls: 0,
    }))
}

async fn execute<F: std::future::Future<Output = Result<echo_contracts::ids::TaskId, echo_agent_api::AgentError>>>(
    backend: &impl AgentBackend,
    sink: &impl TraceSink,
    event: LiveEvent,
    instructions: String,
    cancelled: impl std::future::Future<Output = ()>,
    submit: impl FnOnce(Task) -> F,
) -> Result<Option<LiveEvent>, DelegationError> {
    let Some(task) = task(&event, instructions)? else {
        return Ok(None);
    };
    let delegation_id = task.delegation_id.clone();
    sink.record(TraceEvent::DelegationCreated {
        delegation_id: delegation_id.clone(),
    });
    tokio::pin!(cancelled);
    let id = tokio::select! {
        biased;
        () = &mut cancelled => return Ok(None),
        id = submit(task) => id.map_err(|_| DelegationError::Submission)?,
    };
    loop {
        let event = tokio::select! {
            biased;
            () = &mut cancelled => {
                backend.cancel(&id).await.map_err(|_| DelegationError::MissingTerminal)?;
                return Ok(None);
            }
            event = backend.next_event(&id) => event.ok_or(DelegationError::MissingTerminal)?,
        };
        if !matches!(event, TaskEvent::Progress { .. }) {
            sink.task_terminal(&event);
        }
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
}

#[cfg(test)]
mod delegation_tests {
    use super::*;
    use echo_agent_api::fakes::{BackendScript, FakeBackend};
    use echo_contracts::ids::TaskId;
    use serde_json::json;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Sink(Mutex<Vec<TraceEvent>>);
    impl TraceSink for Sink {
        fn record(&self, event: TraceEvent) {
            self.0.lock().unwrap().push(event);
        }
    }
    async fn execute(
        backend: &impl AgentBackend,
        sink: &impl TraceSink,
        event: LiveEvent,
        instructions: String,
    ) -> Result<Option<LiveEvent>, DelegationError> {
        super::execute(backend, sink, event, instructions, std::future::pending(), |task| {
            backend.submit(task)
        })
        .await
    }
    fn event() -> LiveEvent {
        serde_json::from_value(json!({"type":"session.delegation.created","delegation":{"id":"d-unit","target":"client","opaque":{"x":[1,"雪"]}},"extra":true})).unwrap()
    }
    #[test]
    fn mapping_preserves_entire_event() {
        assert_eq!(
            task(&event(), "instructions".into()).unwrap(),
            Some(Task {
                delegation_id: DelegationId::new("d-unit").unwrap(),
                instructions: "instructions".into(),
                context: serde_json::to_value(event()).unwrap(),
                deadline_seconds: 0,
                max_tool_calls: 0,
            })
        );
        assert_eq!(
            TraceEvent::ToolCall {
                name: "test.echo".into()
            }
            .name(),
            "tools/call"
        );
    }
    #[tokio::test]
    async fn refuses_target_and_ignores_unrelated_without_observation() {
        let backend = FakeBackend::default();
        let sink = Sink::default();
        let mut input = serde_json::to_value(event()).unwrap();
        input["delegation"]["target"] = json!("responses");
        assert_eq!(
            execute(&backend, &sink, serde_json::from_value(input).unwrap(), String::new()).await,
            Err(DelegationError::UnsupportedTarget)
        );
        for input in [
            LiveEvent::Other(json!({"type":"unknown"})),
            LiveEvent::SessionCreated {
                extra: std::collections::BTreeMap::default(),
            },
        ] {
            assert_eq!(execute(&backend, &sink, input, String::new()).await, Ok(None));
        }
        assert_eq!(backend.calls(), vec![]);
        assert_eq!(*sink.0.lock().unwrap(), vec![]);
    }
    #[tokio::test]
    async fn terminal_text_submission_and_missing_terminal_are_exact() {
        let id = TaskId::new("task-unit").unwrap();
        for terminal in [
            Some(TaskEvent::Completed {
                task_id: id.clone(),
                summary: "雪☃".into(),
            }),
            Some(TaskEvent::Failed {
                task_id: id.clone(),
                retryable: false,
                message: "honest failure".into(),
            }),
            None,
        ] {
            let mut events = vec![TaskEvent::Progress {
                task_id: id.clone(),
                message: "working".into(),
            }];
            let expected = terminal.as_ref().map(|e| match e {
                TaskEvent::Completed { summary, .. } => summary.clone(),
                TaskEvent::Failed { message, .. } => message.clone(),
                TaskEvent::Progress { .. } => unreachable!(),
            });
            events.extend(terminal);
            let backend = FakeBackend::new(vec![BackendScript {
                task: task(&event(), "i".into()).unwrap().unwrap(),
                id: id.clone(),
                events,
            }]);
            let sink = Sink::default();
            let result = execute(&backend, &sink, event(), "i".into()).await;
            match expected {
                Some(text) => {
                    assert_eq!(
                        result,
                        Ok(Some(LiveEvent::CommentaryAppend {
                            delegation_id: DelegationId::new("d-unit").unwrap(),
                            text,
                            extra: std::collections::BTreeMap::default()
                        }))
                    );
                    assert_default_sink(&sink);
                }
                None => assert_eq!(result, Err(DelegationError::MissingTerminal)),
            }
            assert_eq!(sink.0.lock().unwrap()[0].name(), "session.delegation.created");
        }
        assert_eq!(
            execute(&FakeBackend::default(), &Sink::default(), event(), String::new()).await,
            Err(DelegationError::Submission)
        );
    }
    fn assert_default_sink(sink: &Sink) {
        assert_eq!(
            *sink.0.lock().unwrap(),
            vec![
                TraceEvent::DelegationCreated {
                    delegation_id: DelegationId::new("d-unit").unwrap()
                },
                TraceEvent::CommentaryAppend {
                    delegation_id: DelegationId::new("d-unit").unwrap()
                },
            ],
            "default observation callbacks preserve the legacy event stream exactly"
        );
    }
}
