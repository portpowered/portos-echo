use super::{
    matrix::{record, scripts, terminal},
    model::{Model, Reply, final_item},
    support::*,
};
use echo_agent_api::{AgentBackend, AgentError, ToolError, ToolHost, ToolResult, ToolSpec, fakes::FakeToolHost};
use echo_agent_loop::{CredentialError, ModelCredential, ResponsesBackend};
use echo_config::LiveMode;
use echo_contracts::{ids::TaskId, secret::Secret, task::TaskEvent};
use reqwest::header::HeaderMap;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Notify;

struct DropSignal(Arc<Notify>);
impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}
struct StalledHost {
    fake: FakeToolHost,
    entered: Notify,
    dropped: Arc<Notify>,
}
impl ToolHost for StalledHost {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>, ToolError> {
        self.fake.list_tools().await
    }
    async fn call_tool(&self, name: &str, args: Value, key: &str) -> Result<ToolResult, ToolError> {
        let result = self.fake.call_tool(name, args, key).await;
        let _guard = DropSignal(self.dropped.clone());
        self.entered.notify_one();
        std::future::pending::<()>().await;
        result
    }
}
#[tokio::test]
async fn f10_f12_stalled_model_timeout_or_cancel() {
    for cancel in [false, true] {
        let mut model = Model::start(vec![Reply::stalled()]).await;
        let host = Arc::new(FakeToolHost::new(Ok(vec![]), vec![]));
        let mut cfg = config();
        cfg.daily_request_cap = 1;
        let backend = ResponsesBackend::loopback(
            cfg,
            LiveMode::ApiKey,
            host.clone(),
            Arc::new(Credentials::default()),
            &model.base,
        )
        .unwrap();
        let mut input = task();
        input.deadline_seconds = 5;
        let id = backend.submit(input).await.unwrap();
        model.entered().await;
        if cancel {
            backend.cancel(&id).await.unwrap();
            backend.cancel(&id).await.unwrap();
        } else {
            tokio::time::pause();
            tokio::time::advance(std::time::Duration::from_secs(5)).await;
        }
        let failure = if cancel { "task cancelled" } else { "task timed out" };
        terminal(&backend, &id, Err(failure)).await;
        if !cancel {
            tokio::time::resume();
        }
        model.closed().await;
        // Cancelled/timed-out dispatched requests remain charged.
        let next = backend.submit(task()).await.unwrap();
        terminal(&backend, &next, Err("daily request limit reached")).await;
        assert_eq!(host.calls(), vec![]);
        assert_eq!(model.finish().await.len(), 1);
        record(if cancel { "F12" } else { "F10" }, 1, 0, Err(failure));
    }
}
#[tokio::test]
async fn f11_f13_stalled_tool_timeout_or_cancel() {
    for cancel in [false, true] {
        let model = Model::start(vec![Reply::body(sse(&[function(0)]))]).await;
        let host = Arc::new(StalledHost {
            fake: FakeToolHost::new(Ok(vec![spec()]), scripts(1)),
            entered: Notify::new(),
            dropped: Arc::new(Notify::new()),
        });
        let mut cfg = config();
        cfg.task_deadline_seconds = 5;
        let backend = ResponsesBackend::loopback(
            cfg,
            LiveMode::ApiKey,
            host.clone(),
            Arc::new(Credentials::default()),
            &model.base,
        )
        .unwrap();
        let id = backend.submit(task()).await.unwrap();
        host.entered.notified().await;
        if cancel {
            backend.cancel(&id).await.unwrap();
            backend.cancel(&id).await.unwrap();
        } else {
            tokio::time::pause();
            tokio::time::advance(std::time::Duration::from_secs(5)).await;
        }
        let failure = if cancel { "task cancelled" } else { "task timed out" };
        terminal(&backend, &id, Err(failure)).await;
        host.dropped.notified().await;
        if !cancel {
            tokio::time::resume();
        }
        assert_eq!(host.fake.calls(), vec![scripts(1).remove(0).call]);
        assert_eq!(model.finish().await.len(), 1);
        record(if cancel { "F13" } else { "F11" }, 1, 1, Err(failure));
    }
}
struct BadCredential(u8);
impl ModelCredential for BadCredential {
    fn headers(
        &self,
        _: LiveMode,
    ) -> impl std::future::Future<Output = Result<Secret<HeaderMap>, CredentialError>> + Send {
        std::future::ready(if self.0 == 0 {
            Err(CredentialError)
        } else {
            let mut headers = HeaderMap::new();
            if self.0 == 2 {
                headers.insert(reqwest::header::AUTHORIZATION, "Bearer synthetic".parse().unwrap());
            }
            Ok(Secret::new(headers))
        })
    }
}
#[tokio::test]
async fn f14_catalog_and_f15_credentials_fail_before_dispatch() {
    let model = Model::start(vec![]).await;
    let host = Arc::new(FakeToolHost::new(Err(ToolError::Transport), vec![]));
    let backend = ResponsesBackend::loopback(
        config(),
        LiveMode::ApiKey,
        host.clone(),
        Arc::new(Credentials::default()),
        &model.base,
    )
    .unwrap();
    let id = backend.submit(task()).await.unwrap();
    terminal(&backend, &id, Err("tool catalog unavailable")).await;
    assert_eq!(host.calls(), vec![]);
    record("F14", 0, 0, Err("tool catalog unavailable"));
    for fail in [0, 1, 2] {
        let host = Arc::new(FakeToolHost::new(Ok(vec![]), vec![]));
        let backend = ResponsesBackend::loopback(
            config(),
            LiveMode::Chatgpt,
            host.clone(),
            Arc::new(BadCredential(fail)),
            &model.base,
        )
        .unwrap();
        let id = backend.submit(task()).await.unwrap();
        terminal(&backend, &id, Err("model credential unavailable")).await;
        assert_eq!(host.calls(), vec![]);
        record("F15", 0, 0, Err("model credential unavailable"));
    }
    assert_eq!(model.finish().await.len(), 0);
}
#[tokio::test]
async fn f18_f19_daily_exhaustion_and_zero() {
    let model = Model::start(vec![
        Reply::body(sse(&[function(0)])),
        Reply::body(sse(&[final_item("hello")])),
    ])
    .await;
    let mut cfg = config();
    cfg.daily_request_cap = 2;
    let host = Arc::new(FakeToolHost::new(Ok(vec![spec()]), scripts(1)));
    let backend = ResponsesBackend::loopback(
        cfg,
        LiveMode::ApiKey,
        host.clone(),
        Arc::new(Credentials::default()),
        &model.base,
    )
    .unwrap();
    let first = backend.submit(task()).await.unwrap();
    terminal(&backend, &first, Ok("hello")).await;
    let second = backend.submit(task()).await.unwrap();
    terminal(&backend, &second, Err("daily request limit reached")).await;
    assert_eq!(host.calls(), vec![scripts(1).remove(0).call]);
    assert_eq!(model.finish().await.len(), 2);
    record("F18", 2, 1, Err("daily request limit reached"));
    let model = Model::start(vec![]).await;
    let mut cfg = config();
    cfg.daily_request_cap = 0;
    let host = Arc::new(FakeToolHost::new(Ok(vec![]), vec![]));
    let backend = ResponsesBackend::loopback(
        cfg,
        LiveMode::ApiKey,
        host.clone(),
        Arc::new(Credentials::default()),
        &model.base,
    )
    .unwrap();
    let id = backend.submit(task()).await.unwrap();
    terminal(&backend, &id, Err("daily request limit reached")).await;
    assert_eq!(host.calls(), vec![]);
    assert_eq!(model.finish().await.len(), 0);
    record("F19", 0, 0, Err("daily request limit reached"));
}
#[tokio::test]
async fn f20_two_tasks_race_last_request() {
    let model = Model::start(vec![Reply::body(sse(&[final_item("hello")]))]).await;
    let mut cfg = config();
    cfg.daily_request_cap = 1;
    let host = Arc::new(FakeToolHost::new(Ok(vec![]), vec![]));
    let backend = ResponsesBackend::loopback(
        cfg,
        LiveMode::ApiKey,
        host.clone(),
        Arc::new(Credentials::default()),
        &model.base,
    )
    .unwrap();
    let (a, b) = tokio::join!(backend.submit(task()), backend.submit(task()));
    let ids = [a.unwrap(), b.unwrap()];
    assert_ne!(ids[0], ids[1]);
    let mut results = Vec::new();
    for id in &ids {
        assert_eq!(
            backend.next_event(id).await,
            Some(TaskEvent::Progress {
                task_id: id.clone(),
                message: "task started".into()
            })
        );
        results.push(backend.next_event(id).await.unwrap());
        assert_eq!(backend.next_event(id).await, None);
    }
    assert_eq!(
        results
            .iter()
            .filter(|e| matches!(e,TaskEvent::Completed{summary,..} if summary=="hello"))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(
                |e| matches!(e,TaskEvent::Failed{retryable:false,message,..} if message=="daily request limit reached")
            )
            .count(),
        1
    );
    assert_eq!(host.calls(), vec![]);
    assert_eq!(model.finish().await.len(), 1);
    record("F20", 1, 0, Ok("hello; other task: daily request limit reached"));
}
#[tokio::test]
async fn f21_same_provider_id_is_scoped_and_cancel_is_isolated() {
    let mut model = Model::start(vec![
        Reply::body(sse(&[function(0)])),
        Reply::stalled(),
        Reply::body(sse(&[function(0)])),
        Reply::body(sse(&[final_item("second")])),
    ])
    .await;
    let mut script = scripts(1);
    let mut second = script[0].clone();
    second.call.idempotency_key = "task-1:call-0".into();
    script.push(second);
    let expected: Vec<_> = script.iter().map(|s| s.call.clone()).collect();
    let host = Arc::new(FakeToolHost::new(Ok(vec![spec()]), script));
    let backend = ResponsesBackend::loopback(
        config(),
        LiveMode::Chatgpt,
        host.clone(),
        Arc::new(Credentials::default()),
        &model.base,
    )
    .unwrap();
    let first = backend.submit(task()).await.unwrap();
    model.entered().await;
    model.entered().await;
    let mut input = task();
    input.instructions = "second task".into();
    input.context = json!({"isolated":true});
    let second = backend.submit(input).await.unwrap();
    assert_ne!(first, second);
    terminal(&backend, &second, Ok("second")).await;
    backend.cancel(&first).await.unwrap();
    terminal(&backend, &first, Err("task cancelled")).await;
    model.closed().await;
    assert_eq!(host.calls(), expected);
    let requests = model.finish().await;
    assert_eq!(requests.len(), 4);
    for (i, request) in requests.iter().enumerate() {
        assert_eq!(request.headers["authorization"], format!("Bearer synthetic-{i}"));
        assert_eq!(request.headers["chatgpt-account-id"], "synthetic");
        let mut history = initial();
        if i >= 2 {
            history[0]["content"][0]["text"] = json!("second task");
            history[1]["content"][0]["text"] = json!("{\"isolated\":true}");
        }
        if i % 2 == 1 {
            history.extend([function(0),json!({"type":"function_call_output","call_id":"call-0","output":serde_json::to_string(&result(0)).unwrap()})]);
        }
        assert_eq!(request.body["input"], json!(history));
    }
    record("F21", 4, 2, Ok("second; other task: task cancelled"));
}
#[tokio::test]
async fn f23_terminal_and_unknown_identity() {
    let model = Model::start(vec![Reply::body(sse(&[final_item("hello")]))]).await;
    let host = Arc::new(FakeToolHost::new(Ok(vec![]), vec![]));
    let backend = ResponsesBackend::loopback(
        config(),
        LiveMode::ApiKey,
        host.clone(),
        Arc::new(Credentials::default()),
        &model.base,
    )
    .unwrap();
    let id = backend.submit(task()).await.unwrap();
    terminal(&backend, &id, Ok("hello")).await;
    backend.cancel(&id).await.unwrap();
    backend.cancel(&id).await.unwrap();
    assert_eq!(backend.next_event(&id).await, None);
    let unknown = TaskId::new("unknown").unwrap();
    assert_eq!(backend.cancel(&unknown).await, Err(AgentError::UnknownTask));
    assert_eq!(backend.next_event(&unknown).await, None);
    assert_eq!(host.calls(), vec![]);
    assert_eq!(model.finish().await.len(), 1);
    record("F23", 1, 0, Ok("hello"));
}

#[tokio::test]
async fn dispatched_failure_keeps_daily_charge() {
    let model = Model::start(vec![Reply {
        status: 500,
        body: "synthetic failure".into(),
        stall: false,
    }])
    .await;
    let host = Arc::new(FakeToolHost::new(Ok(vec![]), vec![]));
    let mut cfg = config();
    cfg.daily_request_cap = 1;
    let backend = ResponsesBackend::loopback(
        cfg,
        LiveMode::ApiKey,
        host.clone(),
        Arc::new(Credentials::default()),
        &model.base,
    )
    .unwrap();
    let first = backend.submit(task()).await.unwrap();
    terminal(&backend, &first, Err("model request failed")).await;
    let second = backend.submit(task()).await.unwrap();
    terminal(&backend, &second, Err("daily request limit reached")).await;
    assert_eq!(host.calls(), vec![]);
    assert_eq!(model.finish().await.len(), 1);
}
