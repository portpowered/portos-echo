//! T3 adapter/MCP boundary; real-pin turn orchestration is proved separately by M4.
use super::journey::Token;
use crate::config;
use echo_agent_api::ToolHost;
use echo_codex::codex_protocol::ToolCall;
use echo_config::{McpAuth, McpServer};
use echo_delegation::codex_dispatch::{Catalog, Dispatcher};
use echo_mcp_client::McpClient;
use echo_spoof_mcp::{CallRecord, Fault, Options, Spoof};
use serde_json::json;
use std::time::Duration;

fn request(index: usize) -> ToolCall {
    ToolCall {
        thread_id: "thread-1".into(),
        turn_id: "turn-1".into(),
        call_id: format!("call-{index}"),
        tool: "tool_0".into(),
        arguments: json!({"text":"delegated echo"}),
        namespace: None,
    }
}

#[tokio::test]
async fn codex_dynamic_catalog_dispatches_one_mcp_attempt_and_reports_http_failure_honestly() {
    for fault in [Fault::None, Fault::Http500] {
        let spoof = Spoof::start(Options {
            fault,
            ..Options::default()
        })
        .await
        .unwrap();
        let (host, catalog) = discovered(&spoof).await;
        let mut dispatcher = Dispatcher::new(host, catalog, "thread-1".into(), config()).unwrap();
        dispatcher.start_turn("thread-1", "turn-1").unwrap();
        let outcome = dispatcher.dispatch(&request(0)).await;
        assert!(outcome.interrupt.is_none());
        let result = serde_json::to_value(outcome.result).unwrap();
        let text = if fault == Fault::None {
            "[{\"text\":\"delegated echo\",\"type\":\"text\"}]"
        } else {
            "tool execution failed"
        };
        assert_eq!(
            result,
            json!({"contentItems":[{"type":"inputText","text":text}],"success":fault == Fault::None})
        );
        assert!(!dispatcher.dispatch(&request(0)).await.result.success);
        let snapshot = spoof.snapshot().unwrap();
        assert_eq!(
            snapshot.calls,
            vec![CallRecord {
                name: "test.echo".into(),
                args: request(0).arguments,
                count: 1
            }]
        );
        assert_eq!(
            snapshot
                .requests
                .iter()
                .filter(|record| record.method == "tools/call")
                .count(),
            1
        );
        spoof.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn codex_nonallowlisted_alias_never_reaches_mcp() {
    let spoof = Spoof::start(Options::default()).await.unwrap();
    let host = McpClient::new(
        McpServer {
            name: "spoof".into(),
            url: spoof.url().into(),
            auth: McpAuth::BearerFile,
            bearer_file: None,
            allow_tools: vec![],
        },
        Token,
        Duration::from_secs(5),
    )
    .unwrap();
    let catalog = Catalog::new(host.list_tools().await.unwrap(), &[]).unwrap();
    assert_eq!(catalog.tools(), []);
    let mut dispatcher = Dispatcher::new(host, catalog, "thread-1".into(), config()).unwrap();
    dispatcher.start_turn("thread-1", "turn-1").unwrap();
    assert!(!dispatcher.dispatch(&request(0)).await.result.success);
    assert_eq!(spoof.snapshot().unwrap().calls, []);
    assert_eq!(
        spoof
            .snapshot()
            .unwrap()
            .requests
            .iter()
            .filter(|record| record.method == "tools/call")
            .count(),
        0
    );
    spoof.shutdown().await.unwrap();
}

async fn discovered(spoof: &Spoof) -> (McpClient<Token>, Catalog) {
    let allowed = ["test.echo", "test.wait", "test.requires_approval", "ui.show_emotion"]
        .map(String::from)
        .to_vec();
    let host = McpClient::new(
        McpServer {
            name: "spoof".into(),
            url: spoof.url().into(),
            auth: McpAuth::BearerFile,
            bearer_file: None,
            allow_tools: allowed.clone(),
        },
        Token,
        Duration::from_secs(5),
    )
    .unwrap();
    let catalog = Catalog::new(host.list_tools().await.unwrap(), &allowed).unwrap();
    assert_eq!(
        catalog
            .tools()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["tool_0", "tool_1", "tool_2", "tool_3"]
    );
    for (index, name) in allowed.iter().enumerate() {
        assert_eq!(catalog.resolve(&format!("tool_{index}")), Some(name.as_str()));
    }
    (host, catalog)
}
