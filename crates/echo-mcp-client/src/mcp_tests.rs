use super::*;
use crate::mcp_wire::{BODY_LIMIT, CATALOG_LIMIT, Sse, envelope, page, result};
use echo_config::McpAuth;
use echo_contracts::secret::Secret;

struct Missing;
impl TokenProvider for Missing {
    fn token(&self) -> impl Future<Output = Result<Secret<String>, TokenError>> {
        std::future::ready(Err(TokenError::Unavailable))
    }
}
fn config(url: &str) -> McpServer {
    McpServer {
        name: "local".into(),
        url: url.into(),
        auth: McpAuth::None,
        bearer_file: None,
        allow_tools: vec!["test.echo".into()],
    }
}
fn host() -> McpClient<Missing> {
    McpClient::new(config("http://127.0.0.1:1/mcp"), Missing, Duration::from_secs(1)).unwrap()
}

#[test]
fn t1_fixed_error_mapping_does_not_retain_remote_diagnostics() {
    for (native, expected) in [
        (McpError::Refused, ToolError::Denied),
        (McpError::TokenUnavailable, ToolError::Unauthorized),
        (McpError::HttpStatus(401), ToolError::Unauthorized),
        (McpError::HttpStatus(403), ToolError::Unauthorized),
        (McpError::Timeout, ToolError::Timeout),
        (McpError::HttpStatus(500), ToolError::Server("http_status:500".into())), // lint-rules: allow-literal fixed HTTP error assertion, never a network address
        (McpError::JsonRpc(-32602), ToolError::Server("json_rpc:-32602".into())),
        (McpError::Protocol, ToolError::Transport),
        (McpError::Transport, ToolError::Transport),
        (
            McpError::InvalidConfiguration,
            ToolError::Server("invalid_configuration".into()),
        ),
        (McpError::Capacity, ToolError::Server("capacity".into())),
    ] {
        assert_eq!(ToolError::from(native), expected);
        assert!(std::error::Error::source(&native).is_none());
    }
    assert_eq!(TokenError::Unavailable.to_string(), "token unavailable");
}

#[test]
fn t1_invalid_configuration_is_rejected_before_use() {
    for url in [
        "bad",
        "file:///tmp/mcp",
        "http://user@127.0.0.1/mcp",
        "http://127.0.0.1/#fragment",
        "http://u:p@127.0.0.1/mcp",
    ] {
        assert!(matches!(
            McpClient::new(config(url), Missing, Duration::from_secs(1)),
            Err(McpError::InvalidConfiguration)
        ));
    }
    assert!(matches!(
        McpClient::new(config("http://127.0.0.1/mcp"), Missing, Duration::ZERO),
        Err(McpError::InvalidConfiguration)
    ));
}

#[tokio::test]
async fn t1_no_network_for_provider_policy_capacity_and_invalid_arguments() {
    let host = host();
    assert_eq!(host.list().await, Err(McpError::TokenUnavailable));
    assert_eq!(host.call("denied", json!({})).await, Err(McpError::Refused));
    assert_eq!(host.call("test.echo", json!([])).await, Err(McpError::Protocol));
    let permit = host.slots.acquire_many(16).await.unwrap();
    assert_eq!(host.list().await, Err(McpError::Capacity));
    drop(permit);
    host.initialized.set(()).unwrap();
    assert_eq!(
        host.call("test.echo", json!({"text":"x".repeat(BODY_LIMIT)})).await,
        Err(McpError::Capacity)
    );
    host.ids.store(u64::MAX, Ordering::Relaxed);
    assert_eq!(host.call("test.echo", json!({})).await, Err(McpError::Capacity));
    send(host.list_tools());
    send(host.call_tool("test.echo", json!({}), "k"));
}

#[test]
fn t2_envelope_requires_version_id_and_exclusive_result_or_typed_error() {
    assert_eq!(
        envelope(br#"{"jsonrpc":"2.0","id":4,"result":{"ok":true}}"#, 4),
        Ok(json!({"ok":true}))
    );
    assert_eq!(
        envelope(
            br#"{"jsonrpc":"2.0","id":4,"error":{"code":-32602,"message":"hostile"}}"#,
            4
        ),
        Err(McpError::JsonRpc(-32602))
    );
    for body in [
        "bad",
        "[]",
        r#"{"jsonrpc":"1.0","id":4,"result":{}}"#,
        r#"{"jsonrpc":"2.0","id":"4","result":{}}"#,
        r#"{"jsonrpc":"2.0","id":5,"result":{}}"#,
        r#"{"jsonrpc":"2.0","id":4}"#,
        r#"{"jsonrpc":"2.0","id":4,"result":{},"error":{}}"#,
        r#"{"jsonrpc":"2.0","id":4,"error":{"message":"x"}}"#,
        r#"{"jsonrpc":"2.0","id":4,"error":{"code":5}}"#,
    ] {
        assert_eq!(envelope(body.as_bytes(), 4), Err(McpError::Protocol), "{body}");
    }
}

#[test]
fn t2_catalog_and_result_preserve_exact_wire_mapping() {
    let schema = json!({"type":"object","properties":{"x":{"type":"string"}}});
    let (tools, cursor) =
        page(json!({"tools":[{"name":"x","description":"desc","inputSchema":schema}],"nextCursor":"next"})).unwrap();
    assert_eq!(
        tools,
        vec![ToolSpec {
            name: "x".into(),
            description: "desc".into(),
            input_schema: schema
        }]
    );
    assert_eq!(cursor, Some("next".into()));
    assert_eq!(page(json!({"tools":[]})), Ok((vec![], None)));
    for value in [
        json!({}),
        json!({"tools":null}),
        json!({"tools":[{"name":"","description":"d","inputSchema":{}}]}),
        json!({"tools":[{"name":"x","description":"d","inputSchema":[]}]}),
        json!({"tools":[],"nextCursor":42}),
    ] {
        assert_eq!(page(value), Err(McpError::Protocol));
    }
    let tool = json!({"name":"x","description":"d","inputSchema":{}});
    assert_eq!(
        page(json!({"tools":vec![tool;CATALOG_LIMIT+1]})),
        Err(McpError::Capacity)
    );
    assert_eq!(
        result(&json!({"content":[{"type":"text","text":"approval_required"}],"isError":true})),
        Ok(ToolResult {
            content: json!([{"type":"text","text":"approval_required"}]),
            is_error: true
        })
    );
    for value in [
        json!({}),
        json!({"content":{},"isError":false}),
        json!({"content":[1],"isError":false}),
        json!({"content":[]}),
        json!({"content":[],"isError":0}),
    ] {
        assert_eq!(result(&value), Err(McpError::Protocol));
    }
}

#[test]
fn t2_sse_segmentation_multiline_comments_and_crlf() {
    let bytes=b": comment\r\nevent: message\r\nid: ignored\r\nretry: 5\r\n\r\ndata: {\"jsonrpc\":\"2.0\",\r\ndata: \"id\":7,\"result\":{\"ok\":true}}\r\n\r\n";
    for split in 1..bytes.len() {
        let mut parser = Sse::default();
        assert_eq!(parser.push(&bytes[..split], 7).unwrap(), None);
        assert_eq!(parser.push(&bytes[split..], 7).unwrap(), Some(json!({"ok":true})));
    }
    let mut parser = Sse::default();
    for byte in &bytes[..bytes.len() - 1] {
        assert_eq!(parser.push(&[*byte], 7).unwrap(), None);
    }
    assert_eq!(
        parser.push(&bytes[bytes.len() - 1..], 7).unwrap(),
        Some(json!({"ok":true}))
    );
}

#[test]
fn t1_sse_rejects_wrong_event_invalid_utf8_and_exhausted_body() {
    let mut parser = Sse::default();
    assert_eq!(
        parser.push(b"event: elicitation\ndata: {}\n\n", 1),
        Err(McpError::Protocol)
    );
    assert_eq!(Sse::default().push(b"data: {}\n\n", 1), Err(McpError::Protocol));
    assert_eq!(Sse::default().push(&[255, b'\n'], 1), Err(McpError::Protocol));
    assert_eq!(
        Sse::default().push(&vec![b'x'; BODY_LIMIT + 1], 1),
        Err(McpError::Capacity)
    );
}

fn send<T: Send>(_: T) {}

#[test]
fn t1_catalog_budget_and_initialization_validation() {
    use crate::mcp_wire::{Catalog, initialized};
    let tool = json!({"name":"x","description":"d","inputSchema":{}});
    let mut catalog = Catalog::default();
    assert_eq!(
        catalog.add(json!({"tools":vec![tool.clone();CATALOG_LIMIT],"nextCursor":"more"})),
        Ok(Some("more".into()))
    );
    assert_eq!(catalog.add(json!({"tools":[tool]})), Err(McpError::Capacity));
    let mut catalog = Catalog::default();
    for page in 0..31 {
        assert_eq!(
            catalog.add(json!({"tools":[],"nextCursor":page.to_string()})),
            Ok(Some(page.to_string()))
        );
    }
    assert_eq!(
        catalog.add(json!({"tools":[],"nextCursor":"last"})),
        Err(McpError::Capacity)
    );
    let valid =
        json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"peer","version":"1"}});
    assert_eq!(initialized(&valid), Ok(()));
    for key in ["protocolVersion", "capabilities", "serverInfo"] {
        let mut invalid = valid.clone();
        invalid[key] = json!(null);
        assert_eq!(initialized(&invalid), Err(McpError::Protocol));
    }
    let mut invalid = valid;
    invalid["serverInfo"]["version"] = json!(null);
    assert_eq!(initialized(&invalid), Err(McpError::Protocol));
}

struct InvalidToken;
impl TokenProvider for InvalidToken {
    fn token(&self) -> impl Future<Output = Result<Secret<String>, TokenError>> {
        std::future::ready(Ok(Secret::new("\n".to_owned())))
    }
}
#[tokio::test]
async fn t1_invalid_bearer_is_rejected_before_network() {
    let host = McpClient::new(config("http://127.0.0.1:1/mcp"), InvalidToken, Duration::from_secs(1)).unwrap();
    assert_eq!(host.list().await, Err(McpError::TokenUnavailable));
}
