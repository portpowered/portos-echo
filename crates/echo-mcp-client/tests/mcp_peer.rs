//! T3: controlled loopback peer proves real POST SSE, pagination and redaction.
#![cfg(test)]
mod peer;
use echo_agent_api::{ToolError, ToolHost};
use echo_mcp_client::McpError;
use peer::{Peer, Reply, host};
use serde_json::json;
use std::sync::{Arc, Mutex};
use tracing::instrument::WithSubscriber;

fn token() -> Arc<Mutex<String>> {
    Arc::new(Mutex::new(format!(
        "local-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )))
}
fn tool(name: &str) -> serde_json::Value {
    json!({"name":name,"description":format!("{name} description"),"inputSchema":{"type":"object","properties":{}}})
}

#[tokio::test]
async fn t3_f16_segmented_post_sse_matches_rpc_id_without_get() {
    let token = token();
    let peer = Peer::start(
        vec![Reply::Sse(
            json!({"content":[{"type":"text","text":"hello"}],"isError":false}),
        )],
        token.clone(),
    )
    .await;
    let host = host(&peer.url, token);
    let result = host.call_tool("test.echo", json!({"text":"hello"}), "k").await.unwrap();
    assert_eq!(result.content, json!([{"type":"text","text":"hello"}]));
    assert!(!result.is_error);
    let records = peer.observations.lock().unwrap().clone();
    assert_eq!(records.len(), 3);
    assert!(
        records
            .iter()
            .all(|r| r.post && r.bearer_equal && r.forbidden_headers == [false; 2])
    );
    assert_eq!(records[2].request["id"], 2);
    peer.shutdown().await;
}

#[tokio::test]
async fn t3_f17_catalog_pages_preserve_order_schema_cursor_and_empty_terminal() {
    let token = token();
    let peer = Peer::start(
        vec![
            Reply::Result(json!({"tools":[tool("one")],"nextCursor":"page-two"})),
            Reply::Result(json!({"tools":[tool("two")]})),
            Reply::Result(json!({"tools":[]})),
        ],
        token.clone(),
    )
    .await;
    let host = host(&peer.url, token);
    let tools = host.list_tools().await.unwrap();
    assert_eq!(
        tools,
        vec![
            echo_agent_api::ToolSpec {
                name: "one".into(),
                description: "one description".into(),
                input_schema: json!({"type":"object","properties":{}})
            },
            echo_agent_api::ToolSpec {
                name: "two".into(),
                description: "two description".into(),
                input_schema: json!({"type":"object","properties":{}})
            }
        ]
    );
    assert_eq!(host.list_tools().await.unwrap(), vec![]);
    let records = peer.observations.lock().unwrap().clone();
    assert_eq!(records.len(), 5);
    assert_eq!(records[3].request["params"], json!({"cursor":"page-two"}));
    peer.shutdown().await;
}

#[tokio::test]
async fn t3_catalog_bounds_fail_without_partial_success() {
    let token = token();
    let peer = Peer::start(
        vec![
            Reply::Result(json!({"tools":[tool("one")],"nextCursor":"repeat"})),
            Reply::Result(json!({"tools":[tool("two")],"nextCursor":"repeat"})),
        ],
        token.clone(),
    )
    .await;
    let host = host(&peer.url, token);
    assert_eq!(host.list().await, Err(McpError::Protocol));
    assert_eq!(peer.observations.lock().unwrap().len(), 4);
    peer.shutdown().await;
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

#[tokio::test]
async fn t3_f12_rotating_bearer_and_hostile_errors_never_disclose_tokens() {
    let token = token();
    let first = token.lock().unwrap().clone();
    let second = format!("{first}-rotated");
    let peer = Peer::start(
        vec![
            Reply::Error(-32602, first.clone()),
            Reply::Raw(500, "application/json", second.clone().into_bytes()),
            Reply::Raw(200, "application/json", first.clone().into_bytes()),
        ],
        token.clone(),
    )
    .await;
    let host = host(&peer.url, token.clone());
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(capture.clone())
        .finish();
    let errors = async {
        let a = host.call_tool("test.echo", json!({}), "k").await.unwrap_err();
        *token.lock().unwrap() = second.clone();
        let b = host.call_tool("test.echo", json!({}), "k").await.unwrap_err();
        let c = host.call_tool("test.echo", json!({}), "k").await.unwrap_err();
        (a, b, c)
    }
    .with_subscriber(subscriber)
    .await;
    assert_eq!(
        errors,
        (
            ToolError::Server("json_rpc:-32602".into()),
            ToolError::Server("http_status:500".into()), // lint-rules: allow-literal fixed HTTP error assertion, never a network address
            ToolError::Transport
        )
    );
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    let output = format!("{logs} {errors:?} {} {} {}", errors.0, errors.1, errors.2);
    for secret in [first, second] {
        assert_eq!(output.matches(&secret).count(), 0);
    }
    let records = peer.observations.lock().unwrap().clone();
    assert_eq!(records.len(), 5);
    assert!(
        records
            .iter()
            .all(|r| r.bearer_equal && r.forbidden_headers == [false; 2])
    );
    peer.shutdown().await;
}

#[tokio::test]
async fn t3_http_body_status_media_and_eof_fail_closed() {
    for (reply, error) in [
        (Reply::Raw(302, "application/json", vec![]), McpError::HttpStatus(302)),
        (Reply::Raw(200, "text/plain", b"hostile".to_vec()), McpError::Protocol),
        (
            Reply::Raw(200, "text/event-stream", b"data: {}".to_vec()),
            McpError::Protocol,
        ),
        (
            Reply::Chunked("application/json", vec![b'x'; 1_048_577]),
            McpError::Capacity,
        ),
        (
            Reply::Raw(200, "application/json", vec![b'x'; 1_048_577]),
            McpError::Capacity,
        ),
    ] {
        let token = token();
        let peer = Peer::start(vec![reply], token.clone()).await;
        let host = host(&peer.url, token);
        assert_eq!(host.call("test.echo", json!({})).await, Err(error));
        assert_eq!(peer.observations.lock().unwrap().len(), 3);
        peer.shutdown().await;
    }
}

#[tokio::test]
async fn t3_initialization_notification_rejects_wrong_status_and_nonempty_body() {
    for (reply, error) in [
        (Reply::Raw(200, "application/json", vec![]), McpError::HttpStatus(200)),
        (Reply::Raw(202, "application/json", vec![b'x']), McpError::Protocol),
    ] {
        let token = token();
        let peer = Peer::with_notification(vec![], token.clone(), Some(reply)).await;
        let host = host(&peer.url, token);
        assert_eq!(host.call("test.echo", json!({})).await, Err(error));
        assert_eq!(peer.observations.lock().unwrap().len(), 2);
        peer.shutdown().await;
    }
}
