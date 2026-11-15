use super::{
    model::{Model, Reply, final_item},
    support::*,
};
use echo_agent_api::{
    AgentBackend, Task, ToolError,
    fakes::{FakeToolHost, ToolCall, ToolScript},
};
use echo_agent_loop::{ModelCredential, ResponsesBackend};
use echo_config::{AgentConfig, LiveMode};
use echo_contracts::{ids::TaskId, task::TaskEvent};
use serde_json::json;
use std::{io::Write, sync::Arc};

pub async fn terminal<
    H: echo_agent_api::ToolHost + Send + Sync + 'static,
    C: ModelCredential + Send + Sync + 'static,
>(
    backend: &ResponsesBackend<H, C>,
    id: &TaskId,
    result: Result<&str, &str>,
) {
    assert_eq!(
        backend.next_event(id).await,
        Some(TaskEvent::Progress {
            task_id: id.clone(),
            message: "task started".into()
        })
    );
    let expected = match result {
        Ok(text) => TaskEvent::Completed {
            task_id: id.clone(),
            summary: text.into(),
        },
        Err(text) => TaskEvent::Failed {
            task_id: id.clone(),
            retryable: false,
            message: text.into(),
        },
    };
    assert_eq!(backend.next_event(id).await, Some(expected));
    assert_eq!(backend.next_event(id).await, None);
}
pub fn scripts(count: usize) -> Vec<ToolScript> {
    (0..count)
        .map(|index| ToolScript {
            call: ToolCall {
                name: "test.echo".into(),
                args: json!({"message":message(index)}),
                idempotency_key: format!("task-0:call-{index}"),
            },
            result: Ok(result(index)),
        })
        .collect()
}
pub fn record(case: &str, requests: usize, tools: usize, result: Result<&str, &str>) {
    writeln!(
        std::io::stdout(),
        "{case} model_requests={requests} tool_calls={tools} terminal={result:?} PASS"
    )
    .unwrap();
}
async fn run(
    case: &str,
    replies: Vec<Reply>,
    scripts: Vec<ToolScript>,
    cfg: AgentConfig,
    task: Task,
    expected: Result<&str, &str>,
    (requests, calls): (usize, usize),
) -> Vec<Request> {
    let model = Model::start(replies).await;
    let expected_calls: Vec<_> = scripts.iter().take(calls).map(|s| s.call.clone()).collect();
    let host = Arc::new(FakeToolHost::new(Ok(vec![spec()]), scripts));
    let backend = ResponsesBackend::loopback(
        cfg,
        LiveMode::ApiKey,
        host.clone(),
        Arc::new(Credentials::default()),
        &model.base,
    )
    .unwrap();
    let id = backend.submit(task).await.unwrap();
    terminal(&backend, &id, expected).await;
    assert_eq!(host.calls(), expected_calls);
    let observed = model.finish().await;
    assert_eq!(observed.len(), requests);
    record(case, requests, calls, expected);
    observed
}
fn replies(turns: usize, finish: bool) -> Vec<Reply> {
    let mut replies: Vec<_> = (0..turns).map(|i| Reply::body(sse(&[function(i)]))).collect();
    if finish {
        replies.push(Reply::body(sse(&[final_item("hello")])));
    }
    replies
}
#[tokio::test]
async fn f04_f05_honest_tool_errors() {
    for (case, output) in [
        ("F04", Err(ToolError::Denied)),
        (
            "F05",
            Ok(echo_agent_api::ToolResult {
                content: json!({"error":"blocked"}),
                is_error: true,
            }),
        ),
    ] {
        let mut script = scripts(1);
        script[0].result = output;
        let received = run(
            case,
            vec![
                Reply::body(sse(&[function(0)])),
                Reply::body(sse(&[final_item("I could not run that tool")])),
            ],
            script,
            config(),
            task(),
            Ok("I could not run that tool"),
            (2, 1),
        )
        .await;
        let expected = if case == "F04" {
            json!({"content":{"error":"denied"},"is_error":true})
        } else {
            json!({"content":{"error":"blocked"},"is_error":true})
        };
        assert_eq!(
            received[1].body["input"][3],
            json!({"type":"function_call_output","call_id":"call-0","output":expected.to_string()})
        );
    }
}
#[tokio::test]
async fn f06_to_f09_tool_caps() {
    run(
        "F06",
        replies(9, false),
        scripts(8),
        config(),
        task(),
        Err("tool call limit reached"),
        (9, 8),
    )
    .await;
    run(
        "F07",
        replies(8, true),
        scripts(8),
        config(),
        task(),
        Ok("hello"),
        (9, 8),
    )
    .await;
    for config_cap in [true, false] {
        let mut cfg = config();
        let mut input = task();
        if config_cap {
            cfg.max_tool_calls = 1;
        } else {
            input.max_tool_calls = 1;
        }
        run(
            "F08",
            replies(2, false),
            scripts(1),
            cfg,
            input,
            Err("tool call limit reached"),
            (2, 1),
        )
        .await;
    }
    let mut input = task();
    input.max_tool_calls = 0;
    run(
        "F09",
        replies(1, false),
        vec![],
        config(),
        input,
        Err("tool call limit reached"),
        (1, 0),
    )
    .await;
}
#[tokio::test]
async fn f16_http_failures_are_not_retried() {
    for status in [401, 500, 302] {
        run(
            "F16",
            vec![Reply {
                status,
                body: "synthetic failure".into(),
                stall: false,
            }],
            vec![],
            config(),
            task(),
            Err("model request failed"),
            (1, 0),
        )
        .await;
    }
}
#[tokio::test]
async fn f17_protocol_failures_never_dispatch() {
    let malformed = json!({"type":"function_call","call_id":"x","name":"tool_0","arguments":"{broken"});
    for body in [
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n".into(),
        sse(&[malformed]),
        "data: {\"type\":\"response.failed\"}\n\n".into(),
        "data: {\"type\":\"error\"}\n\n".into(),
        "data: {\"type\":\"response.incomplete\"}\n\n".into(),
        sse(&[]),
        sse(&[json!({"type":"function_call","call_id":"x","name":"unoffered","arguments":"{}"})]),
    ] {
        run(
            "F17",
            vec![Reply::body(body)],
            vec![],
            config(),
            task(),
            Err("invalid model response"),
            (1, 0),
        )
        .await;
    }
}
#[tokio::test]
async fn f22_duplicate_id_and_f24_serial_batch() {
    run(
        "F22",
        vec![Reply::body(sse(&[function(0)])), Reply::body(sse(&[function(0)]))],
        scripts(1),
        config(),
        task(),
        Err("invalid model response"),
        (2, 1),
    )
    .await;
    let batch = sse(&[function(0), function(1)]);
    let received = run(
        "F24",
        vec![Reply::body(batch.clone()), Reply::body(sse(&[final_item("hello")]))],
        scripts(2),
        config(),
        task(),
        Ok("hello"),
        (2, 2),
    )
    .await;
    for i in 0..2 {
        assert_eq!(
            received[1].body["input"][4 + i],
            json!({"type":"function_call_output","call_id":format!("call-{i}"),"output":serde_json::to_string(&result(i)).unwrap()})
        );
    }
    let mut cfg = config();
    cfg.max_tool_calls = 1;
    run(
        "F24-cap1",
        vec![Reply::body(batch)],
        scripts(1),
        cfg,
        task(),
        Err("tool call limit reached"),
        (1, 1),
    )
    .await;
}
