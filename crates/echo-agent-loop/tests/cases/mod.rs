//! T3 component proof: real public backend and HTTP client, loopback model, `FakeToolHost`.
mod support;
use echo_agent_api::{
    AgentBackend,
    fakes::{FakeToolHost, ToolCall, ToolScript},
};
use echo_agent_loop::ResponsesBackend;
use echo_config::LiveMode;
use echo_contracts::task::TaskEvent;
use serde_json::{Value, json};
use std::io::Write;
use std::sync::{Arc, atomic::Ordering};
use support::*;

async fn journey(case: &str, mode: LiveMode, turns: usize) {
    let summary = if turns == 0 { "hello 世界" } else { "hello" };
    let final_item = json!({"type":"message", "id":"msg-1", "role":"assistant", "content":[
        {"type":"output_text", "text":"hello"}, {"type":"output_text", "text": if turns == 0 { " 世界" } else { "" }}]});
    let reasoning = json!({"type":"reasoning", "id":"reason-1", "encrypted_content":"opaque", "summary":[]});
    let calls: Vec<_> = (0..turns).map(function).collect();
    let mut responses: Vec<_> = calls
        .iter()
        .map(|call| sse(&[reasoning.clone(), call.clone()]))
        .collect();
    responses.push(sse(std::slice::from_ref(&final_item)));
    let server = Server::start(responses).await;
    let catalog = if turns == 0 { vec![] } else { vec![spec()] };
    let scripts: Vec<_> = (0..turns)
        .map(|index| ToolScript {
            call: ToolCall {
                name: "test.echo".into(),
                args: json!({"message":message(index)}),
                idempotency_key: format!("task-0:call-{index}"),
            },
            result: Ok(result(index)),
        })
        .collect();
    let expected_calls: Vec<_> = scripts.iter().map(|script| script.call.clone()).collect();
    let host = Arc::new(FakeToolHost::new(Ok(catalog), scripts));
    let credentials = Arc::new(Credentials::default());
    let backend = ResponsesBackend::loopback(config(), mode, host.clone(), credentials.clone(), &server.base).unwrap();
    let id = backend.submit(task()).await.unwrap();
    assert_eq!(
        backend.next_event(&id).await,
        Some(TaskEvent::Progress {
            task_id: id.clone(),
            message: "task started".into()
        })
    );
    assert_eq!(
        backend.next_event(&id).await,
        Some(TaskEvent::Completed {
            task_id: id.clone(),
            summary: summary.into()
        })
    );
    assert_eq!(backend.next_event(&id).await, None);
    assert_eq!(host.calls(), expected_calls);
    assert_eq!(credentials.0.load(Ordering::SeqCst), turns + 1);
    let requests = server.finish().await;
    assert_requests(&requests, mode, &calls, &reasoning, turns);
    writeln!(
        std::io::stdout(),
        "{case} mode={mode:?} model_requests={} tool_calls={turns} terminal=Completed({summary}) PASS",
        requests.len()
    )
    .unwrap();
}

fn assert_requests(requests: &[Request], mode: LiveMode, calls: &[Value], reasoning: &Value, turns: usize) {
    assert_eq!(requests.len(), turns + 1);
    let mut history = initial();
    let tools = if turns == 0 {
        vec![]
    } else {
        vec![
            json!({"type":"function", "name":"tool_0", "description":"Echo a message", "parameters":spec().input_schema, "strict":false}),
        ]
    };
    for (index, request) in requests.iter().enumerate() {
        let path = match mode {
            LiveMode::Chatgpt => "/backend-api/codex/responses",
            LiveMode::ApiKey => "/v1/responses",
        };
        assert_eq!(request.start, format!("POST {path} HTTP/1.1"));
        assert_eq!(request.headers["authorization"], format!("Bearer synthetic-{index}"));
        assert_eq!(
            request.headers.get("chatgpt-account-id").map(String::as_str),
            if mode == LiveMode::Chatgpt {
                Some("synthetic")
            } else {
                None
            }
        );
        assert_eq!(request.headers["content-type"], "application/json");
        assert_eq!(request.headers["accept"], "text/event-stream");
        assert!(!request.headers.contains_key("openai-alpha"));
        assert_eq!(
            request.body,
            json!({"model":"fixture-model", "stream":true, "input":history, "tools":tools,
            "tool_choice":"auto", "parallel_tool_calls":false, "reasoning":null, "store":false, "include":["reasoning.encrypted_content"]})
        );
        if let Some(call) = calls.get(index) {
            history.extend([reasoning.clone(), call.clone(), json!({"type":"function_call_output", "call_id":format!("call-{index}"), "output":serde_json::to_string(&result(index)).unwrap()})]);
        }
    }
}

#[tokio::test]
async fn f01_one_tool_each_mode() {
    for mode in [LiveMode::Chatgpt, LiveMode::ApiKey] {
        journey("F01", mode, 1).await;
    }
}
#[tokio::test]
async fn f02_two_tool_turns_each_mode() {
    for mode in [LiveMode::Chatgpt, LiveMode::ApiKey] {
        journey("F02", mode, 2).await;
    }
}
#[tokio::test]
async fn f03_empty_catalog_direct_unicode_text() {
    for mode in [LiveMode::Chatgpt, LiveMode::ApiKey] {
        journey("F03", mode, 0).await;
    }
}
