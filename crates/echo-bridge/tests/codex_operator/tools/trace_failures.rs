//! T3: real approval boundary and MCP failure preserve response and effect counts.
use super::*;

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn delegation_trace_configured_declined_and_failed_tools_resolve_once_without_retry() {
    for (accepted, payloads) in [(false, true), (true, true), (false, false), (true, false)] {
        let fixture = Fixture::new("chatgpt");
        std::fs::write(fixture.home.join("mode"), "tool-trace-approval").unwrap();
        let spoof = echo_spoof_mcp::Spoof::start(echo_spoof_mcp::Options::default())
            .await
            .unwrap();
        let mut config = config(&fixture, spoof.url());
        config.trace.payloads = payloads;
        let output = generation(&fixture, config, Some(accepted)).await;
        assert_eq!(output["success"], false);
        let snapshot = spoof.snapshot().unwrap();
        assert_eq!(snapshot.calls.len(), usize::from(accepted));
        if accepted {
            assert_eq!(snapshot.calls[0].name, "test.requires_approval");
            assert_eq!(snapshot.calls[0].args, json!({}));
            assert_eq!(snapshot.calls[0].count, 1);
        }
        let rows = trace(&fixture.directory.path().join("tool-trace"));
        let tools: Vec<_> = rows.iter().filter(|row| row["event"] == "delegation.tool").collect();
        assert_eq!(tools.len(), 2);
        let tool = tools.iter().find(|row| row["kind"] == "mcp").unwrap();
        assert_tool(tool, &output, accepted, payloads);
        let created: Vec<_> = rows.iter().filter(|row| row["event"] == "delegation.created").collect();
        let resolved: Vec<_> = rows
            .iter()
            .filter(|row| row["event"] == "delegation.resolved")
            .collect();
        assert_eq!(created.len(), 1);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0]["delegation_id"], created[0]["delegation_id"]);
        assert_eq!(resolved[0]["outcome"], "completed");
        assert_eq!(resolved[0]["tool_counts"], json!({"mcp":1,"web_search":1}));
        assert_eq!(resolved[0]["handback"], "unobserved");
        for row in [*tool, resolved[0]] {
            use std::io::Write;
            writeln!(std::io::stdout(), "{row}").unwrap();
        }
        spoof.shutdown().await.unwrap();
    }
}

fn assert_tool(tool: &Value, output: &Value, accepted: bool, payloads: bool) {
    assert_eq!(tool["status"], if accepted { "failed" } else { "declined" });
    assert_eq!(tool["name"], "test.requires_approval");
    assert_eq!(tool["delegation_id"], "delegation-7");
    assert_eq!(tool["thread_id"], "fixture-thread");
    assert_eq!(tool["turn_id"], "fixture-turn");
    assert_eq!(tool["item_id"], "fixture-call");
    assert_eq!(tool["call_id"], "fixture-call");
    assert_eq!(tool["request_id"], "fixture-tool");
    assert!(tool["duration_ms"].is_u64());
    assert_eq!(tool.get("request"), payloads.then(|| json!({})).as_ref());
    assert_eq!(tool.get("response"), payloads.then_some(output));
    let text = output["contentItems"][0]["text"].as_str().unwrap();
    if accepted {
        assert!(text.contains("approval_required"), "{output}");
    } else {
        assert_eq!(text, "tool request denied");
    }
}
