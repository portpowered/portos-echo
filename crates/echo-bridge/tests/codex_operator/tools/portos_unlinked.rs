//! T3: a configured Port OS server with no grant never stops a voice call.
//! Controlled child/media/device and a loopback OAuth mock; nothing is linked.
use super::*;

#[tokio::test]
#[ignore = "T3 requires local Python and loopback OAuth"]
async fn t3_portos_unlinked_call_starts_without_server_tools_and_plays_reply() {
    let fixture = Fixture::new("chatgpt");
    // This child asserts thread/start advertises only the two session tools.
    std::fs::write(fixture.home.join("mode"), "tool-unlinked-reply").unwrap();
    let mock = echo_mcp_auth::mock::Mock::bind().await.unwrap();
    let mut config = config(&fixture, &format!("{}/mcp", mock.base));
    config.mcp.server[0].auth = echo_config::McpAuth::PortosDeviceCode;
    config.mcp.client_id = "synthetic-cli".into();
    config.mcp.state_dir = Some(fixture.directory.path().join("grant"));
    config.mcp.operator_listen = ([127, 0, 0, 1], 0).into();
    let server = config.mcp.server[0].name.clone();
    let path = fixture.directory.path().join("unlinked-trace");
    let observer = Observer::default();
    let closed = Arc::new(AtomicUsize::new(0));
    let (stop, stopping) = tokio::sync::oneshot::channel();
    let prepared = Configured::new(config, path.clone(), ([127, 0, 0, 1], 0).into()).unwrap();
    let work = prepared.execute(
        FakeConnector::new(observer.clone()),
        async || Ok(media(&closed)),
        vec![],
        async {
            stopping.await.unwrap();
        },
    );
    let inspect = async {
        let (_control, _screen) = ready(&fixture).await;
        observer.wake("unlinked reply", vec![]);
        while !observer.commands().contains(&echo_link::fake::Command::Stop) {
            tokio::task::yield_now().await;
        }
        observer.finish(Ok(()));
        while !trace(&path).iter().any(|row| row["event"] == "reply.played") {
            tokio::task::yield_now().await;
        }
        stop.send(()).unwrap();
    };
    let (outcome, ()) = Box::pin(tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        tokio::join!(work, inspect)
    }))
    .await
    .unwrap();
    assert_eq!(outcome, Ok(()));
    let rows = trace(&path);
    let count = |event: &str| rows.iter().filter(|row| row["event"] == event).count();
    assert_eq!(count("bridge.recovering") + count("bridge.error"), 0);
    assert_eq!(count("call.realtime_start_sent"), 1);
    assert_eq!(count("mcp.tools_list.completed"), 0);
    let missing: Vec<_> = rows
        .iter()
        .filter(|row| row["event"] == "mcp.tools_list.unavailable")
        .collect();
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0]["server"], server.as_str());
    assert_eq!(missing[0]["reason"], "unlinked");
    assert_eq!(count("mcp.relink_required"), 0);
    super::super::host::released(&fixture).await;
}
