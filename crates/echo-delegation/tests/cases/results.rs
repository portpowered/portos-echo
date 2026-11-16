//! T3: actual tool responses observed once, unchanged at the backend boundary.
use super::{
    journey,
    model::{Model, Reply, final_item},
};
use crate::{Credentials, config, sse};
use echo_config::{LiveMode, McpAuth, McpServer};
use echo_delegation::{DelegationRunner, TraceEvent, TraceSink};
use echo_mcp_client::McpClient;
use echo_spoof_mcp::{Fault, Options, Spoof};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Default)]
struct Sink(Mutex<Vec<Value>>);
impl TraceSink for Sink {
    fn record(&self, event: TraceEvent) {
        self.0.lock().unwrap().push(json!(event.name()));
    }
    fn tool_result(&self, name: &str, content: &Value, is_error: bool) {
        self.0
            .lock()
            .unwrap()
            .push(json!({"name":name,"content":content,"is_error":is_error}));
    }
}

#[tokio::test]
async fn m6_result_observer_preserves_success_error_and_transport_values_and_order() {
    for (name, args, fault, result, observed) in [
        (
            "test.echo",
            json!({"text":"exact 雪"}),
            Fault::None,
            json!({"content":[{"type":"text","text":"exact 雪"}],"is_error":false}),
            true,
        ),
        (
            "test.requires_approval",
            json!({}),
            Fault::None,
            json!({"content":[{"type":"text","text":"approval_required"}],"is_error":true}),
            true,
        ),
        (
            "test.echo",
            json!({"text":"not delivered"}),
            Fault::Http500,
            json!({"content":{"error":"server error: http_status:500"},"is_error":true}), // lint-rules: allow-literal inert tool error text, never dialled
            false,
        ),
    ] {
        check(name, args, fault, result, observed).await;
    }
}

async fn check(name: &str, args: Value, fault: Fault, result: Value, observed: bool) {
    let spoof = Spoof::start(Options::default()).await.unwrap();
    let model = Model::start(vec![
        Reply::body(sse(&[
            json!({"type":"function_call","call_id":"call-result","name":if name == "test.requires_approval" { "tool_2" } else { "tool_0" },"arguments":args.to_string()}),
        ])),
        Reply::body(sse(&[final_item("finished")])),
    ])
    .await;
    let host = Arc::new(
        McpClient::new(
            McpServer {
                name: "spoof".into(),
                url: spoof.url().into(),
                auth: McpAuth::None,
                bearer_file: None,
                allow_tools: vec![name.into()],
            },
            journey::Token,
            Duration::from_secs(5),
        )
        .unwrap(),
    );
    let sink = Arc::new(Sink::default());
    let mut runner = DelegationRunner::new(
        config(),
        LiveMode::ApiKey,
        host,
        Arc::new(Credentials::default()),
        sink.clone(),
        Some(&model.base),
    )
    .unwrap();
    spoof.set_fault(fault).unwrap();
    assert_eq!(
        runner.run(journey::event("d-result"), "fixture".into()).await.unwrap(),
        Some(journey::commentary("d-result", "finished"))
    );
    let requests = model.finish().await;
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].body["input"][3],
        json!({"type":"function_call_output",
        "call_id":"call-result","output":result.to_string()})
    );
    let mut expected = vec![json!("session.delegation.created"), json!("tools/call")];
    if observed {
        expected.push(json!({"name":name,"content":result["content"],"is_error":result["is_error"]}));
    }
    expected.push(json!("session.commentary.append"));
    assert_eq!(*sink.0.lock().unwrap(), expected);
    drop(runner);
    spoof.shutdown().await.unwrap();
}
