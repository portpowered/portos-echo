//! T3 retained Client: public application owner, real model/MCP transports on loopback.
use super::*;
use std::io::Write;

pub(super) fn resolved(path: &Path, id: &str, outcome: &str, handback: &str) -> Value {
    let entries = trace(path);
    let own: Vec<_> = entries.iter().filter(|row| row["delegation_id"] == id).collect();
    assert_eq!(
        own.iter()
            .filter(|row| row["event"] == "session.delegation.created")
            .count(),
        1
    );
    let terminal: Vec<_> = own.iter().filter(|row| row["event"] == "delegation.resolved").collect();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0]["outcome"], outcome);
    assert_eq!(terminal[0]["handback"], handback);
    assert_eq!(terminal[0]["run_id"], own[0]["run_id"]);
    assert!(terminal[0]["duration_ms"].as_u64().is_some());
    writeln!(std::io::stdout(), "{}", terminal[0]).unwrap();
    (*terminal[0]).clone()
}

#[tokio::test]
#[ignore = "T3 controlled application and loopback peers"]
async fn delegation_trace_client_completed_text_does_not_infer_failure_and_shutdown_closes_once() {
    let mut app = Application::start(3, 60).await;
    let mut request = app.request("completed").await;
    respond(&mut request, "task timed out").await.unwrap();
    app.commentary("completed", "task timed out").await;
    app.wait_event("delegation.resolved").await;
    let row = resolved(&app.path(), "completed", "completed", "submitted");
    assert_eq!(row["answer"], json!({"text":"task timed out"}));
    assert_eq!(row["tool_counts"], json!({}));
    let mut pending = app.request("shutdown").await;
    let path = app.path();
    app.stop.send(true).unwrap();
    assert_eq!(app.worker.await.unwrap(), Ok(()));
    assert_eq!(pending.read(&mut [0]).await.unwrap(), 0);
    let row = resolved(&path, "shutdown", "cancelled", "unobserved");
    assert_eq!(row["tool_counts"], json!({}));
    assert!(row.get("answer").is_none());
    assert_eq!(app.spoof.snapshot().unwrap().calls.len(), 0);
    app.spoof.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "T3 controlled application and loopback peers"]
async fn delegation_trace_client_tool_payload_switch_and_error_preserve_backend_result() {
    for payloads in [true, false] {
        for failed in [false, true] {
            let mut app = Application::idle_options(3, 60, false, payloads).await;
            app.wake().await;
            if failed {
                app.spoof.set_fault(echo_spoof_mcp::Fault::Http500).unwrap();
            }
            let mut model = app.request("tool").await;
            let args = json!({"text":"synthetic-result-7","authorization":"synthetic-auth-value",
                "account_id":"synthetic-account-value","url":"http://127.0.0.1/synthetic-source"});
            let call =
                json!({"type":"function_call","call_id":"retained-call","name":"tool_0","arguments":args.to_string()});
            let body = sse(&call);
            model.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            let mut followup = app.model.accept().await.unwrap().0;
            let request = read(&mut followup).await;
            let result: Value = serde_json::from_str(request.body["input"][3]["output"].as_str().unwrap()).unwrap();
            if failed {
                assert_eq!(
                    result,
                    json!({"content":{"error":"server error: http_status:500"},"is_error":true})
                ); // lint-rules: allow-literal inert error text
            } else {
                assert_eq!(
                    result,
                    json!({"content":[{"type":"text","text":"synthetic-result-7"}],"is_error":false})
                );
            }
            respond(&mut followup, "synthetic-answer-7").await.unwrap();
            app.commentary("tool", "synthetic-answer-7").await;
            app.wait_event("delegation.resolved").await;
            check_tool(&app, payloads, failed);
            app.finish_with_calls(1).await;
        }
    }
}

fn check_tool(app: &Application, payloads: bool, failed: bool) {
    let row = resolved(&app.path(), "tool", "completed", "submitted");
    assert_eq!(row["tool_counts"], json!({"mcp":1}));
    let entries = trace(&app.path());
    let tools: Vec<_> = entries.iter().filter(|row| row["event"] == "delegation.tool").collect();
    assert_eq!(tools.len(), 1);
    let tool = tools[0];
    assert_eq!(tool["kind"], "mcp");
    assert_eq!(tool["name"], "test.echo");
    assert_eq!(tool["delegation_id"], "tool");
    assert_eq!(tool["run_id"], "run-1");
    assert!(tool["server"].as_str().is_some());
    assert!(tool["duration_ms"].as_u64().is_some());
    assert_eq!(tool["status"], if failed { "failed" } else { "completed" });
    let text = std::fs::read_to_string(app.path()).unwrap();
    for secret in ["synthetic-auth-value", "synthetic-account-value", "synthetic-mcp-token"] {
        assert!(!text.contains(secret));
    }
    if payloads {
        assert_eq!(tool["request"]["text"], "synthetic-result-7");
        assert_eq!(tool["request"]["url"], "http://127.0.0.1/synthetic-source");
        assert_eq!(tool["request"]["authorization"], "[redacted]");
        assert_eq!(tool["request"]["account_id"], "[redacted]");
        assert_eq!(row["answer"], json!({"text":"synthetic-answer-7"}));
    } else {
        for seed in [
            "synthetic-result-7",
            "synthetic-answer-7",
            "http://127.0.0.1/synthetic-source",
        ] {
            assert!(!text.contains(seed));
        }
        assert!(tool.get("request").is_none() && tool.get("response").is_none());
        assert!(row.get("answer").is_none());
    }
    writeln!(std::io::stdout(), "{tool}").unwrap();
}
