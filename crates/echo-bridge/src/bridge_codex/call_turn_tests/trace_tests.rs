//! T2: decoded pinned events preserve native delegation trace correlation.
use super::*;
use serde_json::Value;
use std::io::Write;

type TracedCall<'a> =
    Call<'a, tokio::io::ReadHalf<tokio::io::DuplexStream>, tokio::io::WriteHalf<tokio::io::DuplexStream>>;

fn traced(io: &mut Io, trace: crate::bridge_trace::Trace) -> TracedCall<'_> {
    let mut call = Call::new(io);
    call.thread = Some("thread-1".into());
    call.phase = Phase::Active;
    let catalog = echo_delegation::codex_dispatch::Catalog::new(vec![], &[]).unwrap();
    call.tool_trace = Some(crate::bridge_trace::ToolTrace::new(trace, &catalog));
    call
}

fn project(call: &mut TracedCall<'_>, method: &str, params: Value) -> Result<Event, Error> {
    let mut envelope = json!({"method":method});
    envelope["params"] = params;
    let raw = serde_json::to_vec(&envelope).unwrap();
    call.project(echo_codex::codex_protocol::decode(&raw).unwrap())
}

fn handoff(id: &str) -> Value {
    json!({"threadId":"thread-1","item":{"type":"handoff_request","handoff_id":id,
        "item_id":"item-1","input_transcript":"synthetic-query-7","active_transcript":[]}})
}

fn trace_rows(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn agent_answers(call: &mut TracedCall<'_>) {
    project(
        call,
        "item/completed",
        json!({"threadId":"foreign","turnId":"turn-1",
        "item":{"type":"agentMessage","text":"foreign answer"}}),
    )
    .unwrap();
    project(
        call,
        "item/completed",
        json!({"threadId":"thread-1","turnId":"turn-1",
        "item":{"type":"agentMessage","phase":"commentary","text":"not final"}}),
    )
    .unwrap();
    project(
        call,
        "item/completed",
        json!({"threadId":"thread-1","turnId":"turn-1",
        "item":{"type":"agentMessage","phase":"final_answer","text":"synthetic-result-7"}}),
    )
    .unwrap();
}

fn traced_tools(io: &mut Io, trace: crate::bridge_trace::Trace) -> TracedCall<'_> {
    let mut call = traced(io, trace.clone());
    let catalog = echo_delegation::codex_dispatch::Catalog::new(
        vec![echo_agent_api::ToolSpec {
            name: "test.echo".into(),
            description: "fixture".into(),
            input_schema: json!({}),
        }],
        &["test.echo".into()],
    )
    .unwrap();
    let mut observer = crate::bridge_trace::ToolTrace::new(trace, &catalog);
    observer.server(Some("spoof"));
    call.tool_trace = Some(observer);
    call
}

#[tokio::test(start_paused = true)]
async fn delegation_trace_early_request_keeps_duration_and_function_duplicate_is_one_tool() {
    for item_first in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let trace = crate::bridge_trace::Trace::open(&path, "api_key").unwrap();
        let (mut io, mut server) = fixture();
        let mut call = traced_tools(&mut io, trace);
        project(&mut call, "thread/realtime/itemAdded", handoff("delegation-7")).unwrap();
        write(
            &mut server,
            json!({"id":7,"method":"item/tool/call","params":{
            "threadId":"thread-1","turnId":"turn-1","callId":"tool-1","tool":"test_echo",
            "arguments":{"text":"synthetic-request"}}}),
        )
        .await;
        assert!(matches!(call.next().await.unwrap(), Event::Tool { .. }));
        tokio::time::advance(Duration::from_millis(7)).await;
        project(
            &mut call,
            "turn/started",
            json!({"threadId":"thread-1","turn":{"id":"turn-1"}}),
        )
        .unwrap();
        let result = serde_json::from_value(
            json!({"contentItems":[{"type":"inputText","text":"synthetic-output"}],"success":true}),
        )
        .unwrap();
        if !item_first {
            call.answer_tool(&echo_codex::codex_protocol::RequestId::Number(7), &result)
                .unwrap();
        }
        project(&mut call, "item/completed", json!({"threadId":"thread-1","turnId":"turn-1",
        "item":{"id":"tool-1","type":"functionCallOutput","name":"test_echo","namespace":"functions","output":"synthetic-output"}})).unwrap();
        if item_first {
            call.answer_tool(&echo_codex::codex_protocol::RequestId::Number(7), &result)
                .unwrap();
        }
        project(
            &mut call,
            "turn/completed",
            json!({"threadId":"thread-1","turn":{"id":"turn-1","status":"completed"}}),
        )
        .unwrap();
        drop(call);
        let rows = trace_rows(&path);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[2]["event"], "delegation.tool");
        assert_eq!(rows[2]["delegation_id"], "delegation-7");
        assert_eq!(rows[2]["thread_id"], "thread-1");
        assert_eq!(rows[2]["turn_id"], "turn-1");
        assert_eq!(rows[2]["name"], "test.echo");
        assert_eq!(rows[2]["server"], "spoof");
        assert_eq!(rows[2]["duration_ms"], 7);
        assert_eq!(rows[2]["request_id"], 7);
        assert_eq!(rows[2]["request"], json!({"text":"synthetic-request"}));
        assert_eq!(rows[2]["call_id"], "tool-1");
        assert_eq!(rows[3]["tool_counts"], json!({"mcp":1}));
        assert_eq!(rows[3]["outcome"], "completed");
        writeln!(std::io::stdout(), "{}", rows[2]).unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn delegation_trace_native_final_agent_and_completed_turn_never_claim_append() {
    for payloads in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let trace = crate::bridge_trace::Trace::open(&path, "api_key")
            .unwrap()
            .with_payloads(payloads);
        trace.tool_run(Some("run-7")).unwrap();
        let (mut io, _server) = fixture();
        let mut call = traced(&mut io, trace.clone());
        project(&mut call, "thread/realtime/itemAdded", handoff("delegation-7")).unwrap();
        project(&mut call, "thread/realtime/itemAdded", handoff("delegation-7")).unwrap();
        project(
            &mut call,
            "turn/started",
            json!({"threadId":"thread-1","turn":{"id":"turn-1"}}),
        )
        .unwrap();
        agent_answers(&mut call);
        tokio::time::advance(Duration::from_millis(23)).await;
        trace.tool_run(Some("different-run")).unwrap();
        for _ in 0..2 {
            project(
                &mut call,
                "turn/completed",
                json!({"threadId":"thread-1","turn":{"id":"turn-1","status":"completed"}}),
            )
            .unwrap();
        }
        call.fail();
        drop(call);
        let rows = trace_rows(&path);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["delegation_id"], "delegation-7");
        assert_eq!(rows[1]["run_id"], "run-7");
        assert_eq!(rows[1]["turn_id"], "turn-1");
        assert_eq!(rows[1]["thread_id"], "thread-1");
        assert_eq!(rows[1]["outcome"], "completed");
        assert_eq!(rows[1]["handback"], "unobserved");
        assert_eq!(rows[1]["duration_ms"], 23);
        assert_eq!(rows[1]["tool_counts"], json!({}));
        assert_eq!(
            rows[0].get("task"),
            payloads.then(|| handoff("delegation-7")["item"].clone()).as_ref()
        );
        assert_eq!(
            rows[1].get("answer"),
            payloads.then(|| json!({"text":"synthetic-result-7"})).as_ref()
        );
        for row in rows {
            writeln!(std::io::stdout(), "{row}").unwrap();
        }
    }
}

fn terminal_case<'a>(call: &mut TracedCall<'_>, case: &'a str) -> (&'a str, Option<&'static str>) {
    match case {
        "failed" | "interrupted" | "timed_out" => {
            project(
                call,
                "turn/completed",
                json!({"threadId":"thread-1","turn":{"id":"turn-1",
            "status":if case == "failed" { "failed" } else { "interrupted" }}}),
            )
            .unwrap();
            (case, (case == "failed").then_some("turn_failed"))
        }
        "realtime_error" => {
            assert!(matches!(
                project(
                    call,
                    "thread/realtime/error",
                    json!({"threadId":"thread-1","message":"untrusted error"})
                ),
                Err(Error::Unavailable)
            ));
            ("failed", Some("realtime_error"))
        }
        "closed" => {
            project(call, "thread/realtime/closed", json!({"threadId":"thread-1"})).unwrap();
            ("failed", Some("realtime_closed"))
        }
        "malformed" => {
            assert!(matches!(
                project(
                    call,
                    "turn/started",
                    json!({"threadId":"thread-1","turn":{"id":"bad\nid"}})
                ),
                Err(Error::Decode)
            ));
            call.fail();
            ("failed", Some("control_failed"))
        }
        _ => ("cancelled", None),
    }
}

#[tokio::test(start_paused = true)]
async fn delegation_trace_native_terminal_causes_and_cleanup_are_exactly_once() {
    for case in [
        "failed",
        "interrupted",
        "timed_out",
        "realtime_error",
        "closed",
        "drop",
        "malformed",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let trace = crate::bridge_trace::Trace::open(&path, "api_key").unwrap();
        let (mut io, _server) = fixture();
        let mut call = traced(&mut io, trace);
        project(
            &mut call,
            "turn/started",
            json!({"threadId":"thread-1","turn":{"id":"turn-1"}}),
        )
        .unwrap();
        project(&mut call, "thread/realtime/itemAdded", handoff("delegation-7")).unwrap();
        if case == "timed_out" {
            call.tool_trace.as_mut().unwrap().timeout("turn-1");
        }
        let (outcome, reason) = terminal_case(&mut call, case);
        drop(call);
        let rows = trace_rows(&path);
        assert_eq!(rows.len(), 2, "{case}");
        assert_eq!(rows[1]["outcome"], outcome, "{case}");
        assert_eq!(rows[1]["handback"], "unobserved");
        assert_eq!(rows[1].get("reason_class").and_then(Value::as_str), reason);
        assert!(rows[1].get("answer").is_none());
        writeln!(std::io::stdout(), "{}", rows[1]).unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn delegation_trace_native_child_eof_keeps_app_server_exit_after_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let trace = crate::bridge_trace::Trace::open(&path, "api_key").unwrap();
    let (mut io, server) = fixture();
    let mut call = traced(&mut io, trace);
    project(&mut call, "thread/realtime/itemAdded", handoff("delegation-7")).unwrap();
    drop(server);
    assert!(matches!(call.next().await, Err(Error::Unavailable)));
    call.stop(deadline()).await.unwrap();
    drop(call);
    let rows = trace_rows(&path);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1]["outcome"], "failed");
    assert_eq!(rows[1]["reason_class"], "app_server_exit");
    assert!(rows[1].get("answer").is_none());
    assert!(rows[1].get("turn_id").is_none());
    writeln!(std::io::stdout(), "{}", rows[1]).unwrap();
}
