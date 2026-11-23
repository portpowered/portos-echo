//! T3: two simultaneous owners, reused item IDs in two turns and a fresh generation.
use super::*;

#[tokio::test]
#[ignore = "T3 controlled children and loopback MCP"]
async fn delegation_trace_configured_isolates_owners_turns_and_replaced_generations() {
    tokio::join!(owner("synthetic-left"), owner("synthetic-right"));
}

async fn owner(server: &str) {
    let fixture = Fixture::new("chatgpt");
    std::fs::write(fixture.home.join("mode"), "tool-trace-isolation").unwrap();
    let spoof = echo_spoof_mcp::Spoof::start(echo_spoof_mcp::Options::default())
        .await
        .unwrap();
    let mut config = config(&fixture, spoof.url());
    config.mcp.server[0].name = server.into();
    for generation_number in 0..2 {
        let result = generation(&fixture, config.clone(), 0, None).await;
        assert_eq!(result["success"], generation_number == 0);
        // The durable policy ledger survives generation replacement: reused
        // external identities produce fresh diagnostic rows, never replayed MCP.
        assert_eq!(spoof.snapshot().unwrap().calls.len(), 2);
        assert_rows(&fixture, server);
        // The first owner has fully reaped and released its home. Reusing the
        // same external IDs must start fresh state in the next generation.
        std::fs::remove_file(fixture.directory.path().join("tool-trace")).unwrap();
        std::fs::remove_file(fixture.home.join("tool-result")).unwrap();
        std::fs::remove_file(fixture.home.join("peer-port")).unwrap();
    }
    spoof.shutdown().await.unwrap();
}

fn assert_rows(fixture: &Fixture, server: &str) {
    let rows = trace(&fixture.directory.path().join("tool-trace"));
    assert_eq!(
        rows.iter().filter(|row| row["event"] == "delegation.created").count(),
        2
    );
    assert_eq!(
        rows.iter().filter(|row| row["event"] == "delegation.resolved").count(),
        2
    );
    assert_eq!(rows.iter().filter(|row| row["event"] == "delegation.tool").count(), 3);
    for (id, turn, counts) in [
        ("delegation-7", "fixture-turn", json!({"mcp":1,"web_search":1})),
        ("delegation-8", "fixture-turn-2", json!({"mcp":1})),
    ] {
        assert_turn(&rows, id, turn, &counts, server);
    }
    assert!(!rows.iter().any(|row| row["name"] == "foreign_tool"));
}

fn assert_turn(rows: &[Value], id: &str, turn: &str, counts: &Value, server: &str) {
    let resolved: Vec<_> = rows
        .iter()
        .filter(|row| row["event"] == "delegation.resolved" && row["delegation_id"] == id)
        .collect();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0]["outcome"], "completed");
    assert_eq!(&resolved[0]["tool_counts"], counts);
    assert_eq!(resolved[0]["turn_id"], turn);
    assert_eq!(resolved[0]["handback"], "unobserved");
    writeln!(std::io::stdout(), "{}", resolved[0]).unwrap();
    assert_tool(rows, id, turn, server);
}

fn assert_tool(rows: &[Value], id: &str, turn: &str, server: &str) {
    let tools: Vec<_> = rows
        .iter()
        .filter(|row| row["event"] == "delegation.tool" && row["delegation_id"] == id && row["kind"] == "mcp")
        .collect();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["item_id"], "fixture-call");
    assert_eq!(tools[0]["turn_id"], turn);
    assert_eq!(tools[0]["server"], server);
    assert_eq!(tools[0]["name"], "test.echo");
    assert_eq!(tools[0]["thread_id"], "fixture-thread");
    assert_eq!(tools[0]["run_id"], "run-1");
    writeln!(std::io::stdout(), "{}", tools[0]).unwrap();
}
