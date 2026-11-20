//! T3: public configured owner, unchanged pin, real WebRTC peer and local provider/MCP.
use super::*;
use std::io::Write as _;

#[tokio::test]
async fn t3_pinned_tools_configured_trace_and_exact_append_with_real_media() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(journey(false)))
        .await
        .unwrap();
}

#[tokio::test]
async fn t3_pinned_tools_configured_failed_result_and_interrupted_zero_append_with_real_media() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(journey(true)))
        .await
        .unwrap();
}

async fn journey(negative: bool) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let spoof = echo_spoof_mcp::Spoof::start(echo_spoof_mcp::Options::default())
        .await
        .unwrap();
    let config = configure(root, &spoof, 1);
    if negative {
        spoof.set_fault(echo_spoof_mcp::Fault::Http500).unwrap();
    }
    let mut provider = Provider::start().await;
    let owner = Configured::new(config.clone(), root.join("trace"), ([127, 0, 0, 1], 0).into())
        .unwrap()
        .with_routes(LocalRoutes::new(&provider.origin, &provider.origin).unwrap());
    let observer = Observer::default();
    let (stop, stopping) = oneshot::channel();
    let work = owner.execute(
        FakeConnector::new(observer.clone()),
        async || {
            Peer::create(PeerConfig {
                interface_allowlist: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
                allow_loopback: true,
                ..PeerConfig::default()
            })
            .await
            .map_err(|_| echo_bridge::Error::Unavailable)
        },
        vec![],
        async {
            stopping.await.unwrap();
        },
    );
    let inspect = inspect(root, &mut provider, &observer, stop, negative);
    let (result, refused) = Box::pin(async { tokio::join!(work, inspect) }).await;
    assert_eq!(result, Ok(()));
    assert_eq!(observer.commands().last(), Some(&LinkCommand::Close));
    check(root, &spoof, negative);
    released(&config);
    provider.finish_call(&refused).await;
    provider.close().await;
    spoof.shutdown().await.unwrap();
}

fn check(root: &Path, spoof: &echo_spoof_mcp::Spoof, negative: bool) {
    assert_trace(
        root,
        "observed",
        if negative { "failed" } else { "completed" },
        "completed",
    );
    assert_trace(root, "refused", "absent", "interrupted");
    assert_eq!(spoof.snapshot().unwrap().calls.len(), 1);
    assert_eq!(
        spoof.snapshot().unwrap().calls[0].args,
        json!({"text":"delegated echo"})
    );
}

async fn inspect(
    root: &Path,
    provider: &mut Provider,
    observer: &Observer,
    stop: oneshot::Sender<()>,
    negative: bool,
) -> Value {
    receipt(root, "bridge.ready").await;
    observer.wake("trace", vec![]);
    let offer = provider.read().await;
    let mut remote = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now().into_std())
        .await
        .unwrap();
    let answer = remote.answer(offer["offer"].as_str().unwrap()).await.unwrap();
    provider.send(json!({"answer":answer})).await;
    let cases = async {
        provider.cycle_result("observed", false, negative).await;
        assert_eq!(provider.read().await, json!({"append":"observed"}));
        receipt(root, "delegation.resolved").await;
        append(
            provider,
            Some(if negative {
                "I couldn't call test echo."
            } else {
                "test echo: delegated echo"
            }),
        )
        .await;
        let refused = interrupted(root, provider).await;
        append(provider, None).await;
        stop.send(()).unwrap();
        stopped(observer).await;
        refused
    };
    tokio::pin!(cases);
    let refused = loop {
        tokio::select! {
            () = step(&mut remote) => (),
            snapshot = &mut cases => break snapshot,
        }
    };
    remote.close();
    refused
}

async fn append(provider: &mut Provider, text: Option<&str>) {
    provider.send(json!({"append_receipts":true})).await;
    let expected = text.map_or_else(
        || json!([]),
        |text| {
            json!([{
                "type":"delegation.context.append","delegation_item_id":"observed",
                "content":[{"type":"input_text","text":text}]
            }])
        },
    );
    let observed = provider.read().await;
    assert_eq!(observed, json!({"append_receipts":expected}));
    writeln!(std::io::stdout(), "{observed}").unwrap();
}

fn assert_trace(root: &Path, id: &str, tool_status: &str, outcome: &str) {
    let own = |event: &str| {
        rows(root, event)
            .into_iter()
            .filter(|row| row["delegation_id"] == id)
            .collect::<Vec<_>>()
    };
    let created = own("delegation.created");
    let resolved = own("delegation.resolved");
    assert_eq!(created.len(), 1);
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0]["outcome"], outcome);
    assert_eq!(resolved[0]["handback"], "unobserved");
    assert_eq!(resolved[0]["run_id"], "run-1");
    assert_eq!(resolved[0]["thread_id"], created[0]["thread_id"]);
    let tools = own("delegation.tool");
    assert_tools(&tools, &resolved[0], tool_status);
    for row in tools.iter().chain(created.iter()).chain(resolved.iter()) {
        writeln!(std::io::stdout(), "{row}").unwrap();
    }
}

fn assert_tools(tools: &[Value], resolved: &Value, tool_status: &str) {
    if tool_status == "absent" {
        assert_eq!(tools.len(), 0);
        assert_eq!(resolved["tool_counts"], json!({}));
        assert!(resolved.get("answer").is_none());
    } else {
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["kind"], "mcp");
        assert_eq!(tools[0]["name"], "test.echo");
        assert_eq!(tools[0]["status"], tool_status);
        assert_eq!(tools[0]["request"], json!({"text":"delegated echo"}));
        assert_eq!(tools[0]["thread_id"], resolved["thread_id"]);
        assert_eq!(tools[0]["turn_id"], resolved["turn_id"]);
        assert_eq!(resolved["tool_counts"], json!({"mcp":1}));
        assert_eq!(
            resolved["answer"]["text"],
            if tool_status == "failed" {
                "I couldn't call test echo."
            } else {
                "test echo: delegated echo"
            }
        );
    }
}
