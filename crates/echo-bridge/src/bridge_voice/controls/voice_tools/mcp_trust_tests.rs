//! T3 configured OAuth/MCP clients against controlled loopback HTTPS peers.
use super::*;
use crate::bridge_mcp::bridge_component::{
    TlsPeer, approve, assert_mcp_trust_refused, assert_private, assert_tool_receipts, drive,
};
use crate::{bridge_health::Health, bridge_mcp::bridge_operations::Operation};

#[tokio::test(start_paused = true)]
async fn t3_portos_explicit_relink_clears_one_scope_diagnosis_and_restores_control() {
    drive(async {
        let peer = TlsPeer::with_scopes(false, echo_config::McpConfig::default().scopes).await;
        let screen = Screen::bind(None).await.unwrap();
        let health = Health::bind(([127, 0, 0, 1], 0).into()).await.unwrap();
        let path = peer.directory.path().join("trace");
        let mut trace = Trace::open(&path, "api_key").unwrap();
        let mut tools = Tools::new(&peer.config, Arc::new(|| 0));
        tools.open(&peer.config, &screen, &mut trace, None).await.unwrap();
        health.set_mcp(tools.grant()).unwrap();
        let owner = tools.runtime.as_ref().unwrap();
        assert_eq!(health_response(&health).await["mcp"], "unlinked");
        relink(owner).await;
        let host = tools.host.as_ref().unwrap();
        let (a, b) = tokio::join!(
            host.call_tool("control", json!({}), "1:t1:u1:a"),
            host.call_tool("control", json!({}), "1:t1:u1:b")
        );
        let expected = ToolResult {
            is_error: true,
            content: json!([{"type":"text","text":
            "mcp.scope_insufficient: control requires message:send. Link Echo to Port OS again to control devices."}]),
        };
        assert_eq!(a, Ok(expected.clone()));
        assert_eq!(b, Ok(expected));
        assert_eq!(health_response(&health).await["mcp"], "relink_for_scope");
        assert_eq!(health_response(&health).await["relink_required"], true);
        health.state.send_modify(|state| state.relink_required = true);
        relink(owner).await;
        assert_eq!(health_response(&health).await["mcp"], "linked");
        assert_eq!(health_response(&health).await["relink_required"], true);
        let action = host.call_tool("control", json!({}), "1:t1:u1:c").await.unwrap();
        assert_eq!(action.content, json!([{"type":"text","text":"accepted"}]));
        assert!(!action.is_error);
        assert_eq!(
            host.call_tool("control", json!({}), "1:t1:u1:c").await,
            Err(ToolError::Denied)
        );
        let rows = scope_rows(&path);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["tool"], "control");
        assert_eq!(rows[0]["missing_scope"], "message:send");
        tools.close().await;
        health.close().await;
        assert_eq!(
            peer.finish().await,
            json!({"link":2,"token":2,"catalog":0,"query":0,"control":3,"effects":1})
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn t3_portos_generic_forbidden_and_unauthorized_preserve_auth_behavior() {
    drive(async {
        let peer = TlsPeer::start(false).await;
        let screen = Screen::bind(None).await.unwrap();
        let path = peer.directory.path().join("trace");
        let mut trace = Trace::open(&path, "api_key").unwrap();
        let mut tools = Tools::new(&peer.config, Arc::new(|| 0));
        tools.open(&peer.config, &screen, &mut trace, None).await.unwrap();
        let owner = tools.runtime.as_ref().unwrap();
        relink(owner).await;
        let host = tools.host.as_ref().unwrap();
        for mode in ["forbidden", "unknown", "unauthorized"] {
            let key = format!("1:t1:u{}:{mode}", mode.len());
            assert_eq!(
                host.call_tool("control", json!({"mode":mode}), &key).await,
                Err(ToolError::Unauthorized)
            );
            assert_eq!(
                owner.grant.health_status(),
                if mode == "unauthorized" { "unlinked" } else { "linked" }
            );
        }
        assert_eq!(
            host.call_tool("endpoint_query", json!({}), "1:t1:u5:query").await,
            Err(ToolError::Unauthorized)
        );
        assert_eq!(scope_rows(&path).len(), 0);
        tools.close().await;
        assert_eq!(
            peer.finish().await,
            json!({"link":1,"token":1,"catalog":0,"query":0,"control":3})
        );
    })
    .await;
}

async fn relink(owner: &bridge_mcp::Runtime) {
    let mut receipt = owner.operator.start(Operation::Link).ok().unwrap();
    assert!(matches!(
        receipt.progress.recv().await,
        Some(echo_screen_contract::UiState::LinkCode { .. })
    ));
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(receipt.result.await.unwrap(), Ok("linked"));
}

async fn health_response(health: &Health) -> Value {
    reqwest::get(format!("http://127.0.0.1:{}/healthz", health.address.port()))
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn scope_rows(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| row["event"] == "mcp.scope_insufficient")
        .collect()
}

#[tokio::test(start_paused = true)]
async fn t3_mcp_configured_https_link_catalog_query_and_reload() {
    drive(async {
        let peer = TlsPeer::start(false).await;
        let config = &peer.config;
        let screen = Screen::bind(None).await.unwrap();
        let mut trace = Trace::open(&peer.directory.path().join("trace"), "api_key").unwrap();
        let mut tools = Tools::new(config, Arc::new(|| 0));
        tools.open(config, &screen, &mut trace, None).await.unwrap();
        approve(tools.runtime.as_ref().unwrap()).await;
        let path = config.mcp.state_dir.as_ref().unwrap().join("grant.json");
        let stored = std::fs::read(&path).unwrap();
        assert_private(&path);
        let host = tools.host.as_ref().unwrap();
        let specs = host.list_tools().await.unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "endpoint_query");
        assert_eq!(specs[0].input_schema, json!({"type":"object","properties":{}}));
        let result = host.call_tool("endpoint_query", json!({}), "1:t1:u1:c").await.unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content, json!([{"type":"text","text":"synthetic-query-result"}]));
        assert_mcp_trust_refused(tools.runtime.as_ref().unwrap(), config).await;
        tools.close().await;
        tools.open(config, &screen, &mut trace, None).await.unwrap();
        assert_eq!(
            tools.host.as_ref().unwrap().list_tools().await.unwrap()[0].name,
            "endpoint_query"
        );
        assert_eq!(std::fs::read(path).unwrap(), stored);
        tools.close().await;
        assert_eq!(peer.finish().await, json!({"link":1,"token":1,"catalog":2,"query":1}));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn t3_mcp_receipts_count_returns_exclude_payloads_and_never_replay() {
    drive(async {
        let mut peer = TlsPeer::start(false).await;
        let screen = Screen::bind(None).await.unwrap();
        let path = peer.directory.path().join("trace");
        let mut trace = Trace::open(&path, "api_key").unwrap();
        let mut tools = Tools::new(&peer.config, Arc::new(|| 0));
        tools.open(&peer.config, &screen, &mut trace, None).await.unwrap();
        approve(tools.runtime.as_ref().unwrap()).await;
        tools.catalog().await.unwrap();
        tools.catalog().await.unwrap(); // The process catalog is cached.
        let host = tools.host.as_ref().unwrap();
        let key = "1:t1:u1:c";
        let result = host.call_tool("endpoint_query", json!({}), key).await.unwrap();
        assert_eq!(result.content, json!([{"type":"text","text":"synthetic-query-result"}]));
        assert!(!result.is_error);
        assert_eq!(
            host.call_tool("endpoint_query", json!({}), key).await,
            Err(ToolError::Denied)
        );
        assert!(
            host.call_tool("endpoint_query", json!({"mode":"error"}), "1:t1:u1:e")
                .await
                .unwrap()
                .is_error
        );
        for (mode, key, error) in [
            ("http", "1:t1:u1:h", ToolError::Server("http_status:503".into())), // lint-rules: allow-literal typed HTTP status assertion, never a network address
            ("malformed", "1:t1:u1:m", ToolError::Transport),
        ] {
            assert_eq!(
                host.call_tool("endpoint_query", json!({"mode":mode}), key).await,
                Err(error)
            );
        }
        {
            let pending = host.call_tool("endpoint_query", json!({"mode":"pending"}), "1:t1:u1:p");
            tokio::pin!(pending);
            tokio::select! {
                () = peer.pending() => (),
                result = &mut pending => panic!("pending tool returned: {result:?}"),
            }
        } // Cancellation drops the actual in-flight HTTP future.
        tools.close().await;
        assert_tool_receipts(
            &path,
            &["c", "e", "h", "m", "p"],
            &[(1, "success"), (2, "error"), (3, "error"), (4, "error")],
        );
        assert_eq!(peer.finish().await, json!({"link":1,"token":1,"catalog":1,"query":5}));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn t3_mcp_receipts_correlate_overlapping_calls_completed_out_of_order() {
    drive(async {
        let mut peer = TlsPeer::start(false).await;
        let screen = Screen::bind(None).await.unwrap();
        let path = peer.directory.path().join("trace");
        let mut trace = Trace::open(&path, "api_key").unwrap();
        let mut tools = Tools::new(&peer.config, Arc::new(|| 0));
        tools.open(&peer.config, &screen, &mut trace, None).await.unwrap();
        approve(tools.runtime.as_ref().unwrap()).await;
        tools.catalog().await.unwrap();
        let host = tools.host.as_ref().unwrap();
        {
            let first = host.call_tool("endpoint_query", json!({"mode":"pending"}), "1:t1:u1:a");
            tokio::pin!(first);
            tokio::select! {
                () = peer.pending() => (),
                result = &mut first => panic!("pending tool returned: {result:?}"),
            }
            assert!(
                !host
                    .call_tool("endpoint_query", json!({}), "1:t1:u1:b")
                    .await
                    .unwrap()
                    .is_error
            );
            peer.release().await;
            assert!(!first.await.unwrap().is_error);
        }
        tools.close().await;
        assert_tool_receipts(&path, &["a", "b"], &[(2, "success"), (1, "success")]);
        assert_eq!(peer.finish().await, json!({"link":1,"token":1,"catalog":1,"query":2}));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn t3_mcp_configured_oauth_refuses_missing_invalid_untrusted_and_wrong_hostname() {
    drive(async {
        let mut peer = TlsPeer::start(false).await;
        let unrelated = TlsPeer::start(false).await;
        let wrong_host = TlsPeer::start(true).await;
        let trusted = peer.config.codex.ca_file.clone();
        let invalid = peer.directory.path().join("invalid.pem");
        std::fs::write(&invalid, b"invalid PEM").unwrap();
        let screen = Screen::bind(None).await.unwrap();
        let mut trace = Trace::open(&peer.directory.path().join("trace"), "api_key").unwrap();
        for ca in [
            None,
            Some(invalid),
            unrelated.config.codex.ca_file.clone(),
            Some(peer.directory.path().join("missing.pem")),
        ] {
            peer.config.codex.ca_file = ca;
            let mut tools = Tools::new(&peer.config, Arc::new(|| 0));
            assert!(matches!(
                tools.open(&peer.config, &screen, &mut trace, None).await,
                Err(Error::Input | Error::Mcp(echo_mcp_auth::AuthError::Transport))
            ));
            assert!(!peer.config.mcp.state_dir.as_ref().unwrap().exists());
            tools.close().await;
        }
        peer.config.codex.ca_file = trusted;
        let mut tools = Tools::new(&wrong_host.config, Arc::new(|| 0));
        assert!(matches!(
            tools.open(&wrong_host.config, &screen, &mut trace, None).await,
            Err(Error::Mcp(echo_mcp_auth::AuthError::Transport))
        ));
        assert!(!wrong_host.config.mcp.state_dir.as_ref().unwrap().exists());
        tools.close().await;
        let zero = json!({"link":0,"token":0,"catalog":0,"query":0});
        assert_eq!(peer.finish().await, zero);
        assert_eq!(unrelated.finish().await, zero);
        assert_eq!(wrong_host.finish().await, zero);
    })
    .await;
}
