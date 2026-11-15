//! T1 lifecycle checks use only in-memory futures, never sockets.
use super::*;
use echo_agent_api::{ToolError, ToolResult, ToolSpec};
use echo_contracts::ids::DelegationId;
use serde_json::{Value, json};
use tokio::sync::Notify;

struct Signals {
    catalog: bool,
    entered: Notify,
    dropped: Arc<Notify>,
}
struct Guard(Arc<Notify>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}
impl Signals {
    async fn stall(&self) {
        let _guard = Guard(self.dropped.clone());
        self.entered.notify_one();
        std::future::pending::<()>().await;
    }
}
impl ToolHost for Signals {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>, ToolError> {
        if self.catalog {
            self.stall().await;
        }
        Ok(vec![])
    }
    fn call_tool(&self, _: &str, _: Value, _: &str) -> impl Future<Output = Result<ToolResult, ToolError>> + Send {
        std::future::ready(Err(ToolError::Denied))
    }
}
impl ModelCredential for Signals {
    async fn headers(&self, _: LiveMode) -> Result<Secret<HeaderMap>, CredentialError> {
        self.stall().await;
        Err(CredentialError)
    }
}
fn setup(catalog: bool) -> (Arc<Signals>, ResponsesBackend<Signals, Signals>, Task) {
    let signals = Arc::new(Signals {
        catalog,
        entered: Notify::new(),
        dropped: Arc::new(Notify::new()),
    });
    let config = AgentConfig {
        model: Some("fixture-model".into()),
        max_tool_calls: 8,
        task_deadline_seconds: 5,
        daily_request_cap: 1,
    };
    let backend = ResponsesBackend::new(config, LiveMode::ApiKey, signals.clone(), signals.clone()).unwrap();
    let task = Task {
        delegation_id: DelegationId::new("unit-delegation").unwrap(),
        instructions: String::new(),
        context: json!({}),
        deadline_seconds: 60,
        max_tool_calls: 8,
    };
    (signals, backend, task)
}
#[tokio::test]
async fn cancellation_timeout_and_shutdown_drop_catalog_and_credentials() {
    for catalog in [true, false] {
        for stop in ["cancel", "timeout", "shutdown"] {
            let (signals, backend, task) = setup(catalog);
            let id = backend.submit(task).await.unwrap();
            signals.entered.notified().await;
            if stop == "shutdown" {
                drop(backend);
            } else {
                if stop == "cancel" {
                    backend.cancel(&id).await.unwrap();
                } else {
                    tokio::time::pause();
                    tokio::time::advance(std::time::Duration::from_secs(5)).await;
                }
                assert_eq!(
                    backend.next_event(&id).await,
                    Some(TaskEvent::Progress {
                        task_id: id.clone(),
                        message: "task started".into()
                    })
                );
                assert_eq!(
                    backend.next_event(&id).await,
                    Some(TaskEvent::Failed {
                        task_id: id.clone(),
                        retryable: false,
                        message: if stop == "cancel" {
                            "task cancelled"
                        } else {
                            "task timed out"
                        }
                        .into()
                    })
                );
                assert!(lock(&backend.tasks).get(&id).unwrap().is_none());
                if stop == "timeout" {
                    tokio::time::resume();
                }
                assert_eq!(backend.budget.reserve(), Ok(()));
            }
            signals.dropped.notified().await;
        }
    }
}
#[tokio::test]
async fn zero_task_deadline_does_not_poll_catalog_or_credentials() {
    let (_signals, backend, mut task) = setup(true);
    task.deadline_seconds = 0;
    let id = backend.submit(task).await.unwrap();
    backend.next_event(&id).await;
    assert_eq!(
        backend.next_event(&id).await,
        Some(TaskEvent::Failed {
            task_id: id.clone(),
            retryable: false,
            message: "task timed out".into()
        })
    );
    assert_eq!(backend.next_event(&id).await, None);
    assert_eq!(backend.cancel(&id).await, Ok(()));
    assert_eq!(backend.budget.reserve(), Ok(()));
}
