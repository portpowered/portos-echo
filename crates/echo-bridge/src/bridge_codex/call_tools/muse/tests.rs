use super::*;
use echo_agent_api::{AgentError, ToolError, ToolHost, ToolSpec};
use echo_contracts::{ids::TaskId, task::TaskEvent};
use echo_delegation::codex_dispatch::{Catalog, Dispatcher};
use serde_json::json;
use std::future::{Future, ready};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Default)]
struct BackendFake {
    tasks: Mutex<Vec<Task>>,
    cancels: Mutex<Vec<TaskId>>,
    poll: Notify,
    pending: bool,
}
impl AgentBackend for BackendFake {
    fn submit(&self, task: Task) -> impl Future<Output = Result<TaskId, AgentError>> + Send + '_ {
        self.tasks.lock().unwrap().push(task);
        ready(Ok(TaskId::new("task-7").unwrap()))
    }

    async fn next_event(&self, id: &TaskId) -> Option<TaskEvent> {
        if self.pending {
            self.poll.notify_one();
            std::future::pending().await
        } else {
            Some(TaskEvent::Completed {
                task_id: id.clone(),
                summary: "Muse test job completed".into(),
            })
        }
    }

    fn cancel<'a>(&'a self, id: &'a TaskId) -> impl Future<Output = Result<(), AgentError>> + Send + 'a {
        self.cancels.lock().unwrap().push(id.clone());
        ready(Ok(()))
    }
}

struct UnreachableMcp(Arc<std::sync::atomic::AtomicUsize>);
impl ToolHost for UnreachableMcp {
    fn list_tools(&self) -> impl Future<Output = Result<Vec<ToolSpec>, ToolError>> + Send + '_ {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        ready(Err(ToolError::Denied))
    }

    fn call_tool<'a>(
        &'a self,
        _: &'a str,
        _: Value,
        _: &'a str,
    ) -> impl Future<Output = Result<echo_agent_api::ToolResult, ToolError>> + Send + 'a {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        ready(Err(ToolError::Denied))
    }
}

fn invocation(root: &std::path::Path, backend: Arc<BackendFake>, remaining: Duration) -> Invocation<BackendFake> {
    Invocation {
        backend,
        trace: Trace::open(&root.join("trace"), "api_key").unwrap(),
        run: "run-7".into(),
        deadline: Instant::now() + remaining,
        delegation: DelegationId::new("handoff-7").unwrap(),
    }
}

fn assert_reply(actual: ToolResult, text: &str, success: bool) {
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        json!({
            "success": success,
            "contentItems":[{"type":"inputText","text":text}]
        })
    );
}

#[tokio::test(start_paused = true)]
async fn muse_tool_correlates_and_rounds_remaining_run_time_and_refuses_replay() {
    let root = tempfile::tempdir().unwrap();
    let backend = Arc::new(BackendFake::default());
    let tool = crate::bridge_muse::tool_spec();
    let effects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let catalog = Catalog::new(vec![tool], &[crate::bridge_muse::TOOL.into()]).unwrap();
    let mut dispatcher = Dispatcher::new(UnreachableMcp(effects.clone()), catalog, "thread-7".into()).unwrap();
    dispatcher.start_turn("thread-7", "turn-7").unwrap();
    let call = serde_json::from_value(json!({"threadId":"thread-7","turnId":"turn-7",
        "callId":"call-7","tool":crate::bridge_muse::TOOL,"arguments":{"request":" Read the test light state. "}}))
    .unwrap();
    let invoke = async |args: &Value| {
        let (_cancel, stop) = oneshot::channel();
        invoke(
            Some(invocation(root.path(), backend.clone(), Duration::from_millis(4001))),
            args,
            stop,
        )
        .await
    };
    let output = dispatcher.dispatch_local(&call, crate::bridge_muse::TOOL, invoke).await;
    assert_reply(output, "Muse test job completed", true);
    let replay = dispatcher.dispatch_local(&call, crate::bridge_muse::TOOL, invoke).await;
    assert_reply(replay, "stale or duplicate tool request", false);
    assert_eq!(effects.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(
        *backend.tasks.lock().unwrap(),
        vec![Task {
            delegation_id: DelegationId::new("handoff-7").unwrap(),
            instructions: "Read the test light state.".into(),
            context: Value::Null,
            deadline_seconds: 5,
            max_tool_calls: 8,
        }]
    );
    assert_eq!(*backend.cancels.lock().unwrap(), vec![]);
}

#[tokio::test(start_paused = true)]
async fn muse_tool_rejects_invalid_expired_or_unowned_requests_before_submit() {
    let root = tempfile::tempdir().unwrap();
    let backend = Arc::new(BackendFake::default());
    for arg in [
        json!({}),
        json!({"request":"  "}),
        json!({"request":3}),
        json!({"request":"test","task_id":"foreign"}),
    ] {
        let (_cancel, stop) = oneshot::channel();
        let ctx = invocation(root.path(), backend.clone(), Duration::from_secs(2));
        assert_reply(invoke(Some(ctx), &arg, stop).await, "tool request refused", false);
    }
    let args = json!({"request":"test"});
    let (_cancel, stop) = oneshot::channel();
    assert_reply(
        invoke(None::<Invocation<BackendFake>>, &args, stop).await,
        "tool request refused",
        false,
    );
    let (_cancel, stop) = oneshot::channel();
    let ctx = invocation(root.path(), backend.clone(), Duration::ZERO);
    assert_reply(invoke(Some(ctx), &args, stop).await, "tool request cancelled", false);
    assert_eq!(*backend.tasks.lock().unwrap(), vec![]);
}

#[tokio::test(start_paused = true)]
async fn muse_tool_cancelled_run_joins_the_task_and_does_not_return_success() {
    let root = tempfile::tempdir().unwrap();
    let backend = Arc::new(BackendFake {
        pending: true,
        ..BackendFake::default()
    });
    let ctx = invocation(root.path(), backend.clone(), Duration::from_secs(60));
    let (cancel, stop) = oneshot::channel();
    let args = json!({"request":"test"});
    let (result, ()) = tokio::join!(invoke(Some(ctx), &args, stop), async {
        backend.poll.notified().await;
        cancel.send(()).unwrap();
    });
    assert_reply(result, "tool request cancelled", false);
    assert_eq!(*backend.cancels.lock().unwrap(), vec![TaskId::new("task-7").unwrap()]);
    let trace = std::fs::read_to_string(root.path().join("trace")).unwrap();
    let entries: Vec<Value> = trace.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(
        entries,
        vec![json!({"event":"muse.submit","run_id":"run-7","elapsed_ms":0,
        "task_id":"task-7","delegation_id":"handoff-7","provider_auth_mode":"api_key"})]
    );
}
