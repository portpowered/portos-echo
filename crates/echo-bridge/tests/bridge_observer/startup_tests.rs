//! T3 startup boundaries through the prebuilt bridge and controlled IPv4 peers.
use super::*;
use tokio::net::{TcpListener, TcpSocket, TcpStream};

#[tokio::test]
#[ignore = "T3 uses a prebuilt bridge and a delayed local listener"]
async fn startup_port_is_advertised_before_listen_without_premature_ready() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let socket = TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let address = socket.local_addr().unwrap();
    assert!(TcpStream::connect(address).await.is_err());
    let fake = FakeLive::bind(FakeScript {
        events: vec![],
        response: FakeResponse::Answer,
    })
    .await
    .unwrap();
    let mut bridge = Process::spawn(&mut bridge(&address.port().to_string(), &fake.base_url(), &path));
    // The product's flushed startup trace is the release signal, not a sleep.
    tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        loop {
            if std::fs::read_to_string(&path).is_ok_and(|text| text.contains("bridge.started")) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        trace(&path)
            .iter()
            .filter(|row| row["event"] == "wyoming.connected")
            .count(),
        0
    );
    assert!(bridge.child.try_wait().unwrap().is_none());
    let listener = socket.listen(1).unwrap();
    let (mut stream, _) = tokio::time::timeout(OBSERVATION_TIMEOUT, listener.accept())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bridge_short::read(&mut stream).await.0["type"], "run-satellite");
    assert_eq!(bridge_short::read(&mut stream).await.0["type"], "describe");
    bridge_short::send(&mut stream, "info", json!({"satellite":{"name":"synthetic"}})).await;
    assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
    assert_eq!(
        trace(&path)
            .iter()
            .filter(|row| row["event"] == "wyoming.connected")
            .count(),
        1
    );
    bridge.finish(0).await;
    drop(stream);
    // This test owns the listener directly; dropping it releases that owner.
    drop(listener);
    bridge.assert_collected();
    fake.shutdown().await;
}

#[tokio::test]
#[ignore = "T3 uses a prebuilt bridge and a stalled local registration"]
async fn startup_reachable_peer_without_info_exits_without_ready() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let fake = FakeLive::bind(FakeScript {
        events: vec![],
        response: FakeResponse::Answer,
    })
    .await
    .unwrap();
    let mut bridge = Process::spawn(&mut bridge(&address.port().to_string(), &fake.base_url(), &path));
    let (mut stream, _) = listener.accept().await.unwrap();
    assert_eq!(bridge_short::read(&mut stream).await.0["type"], "run-satellite");
    assert_eq!(bridge_short::read(&mut stream).await.0["type"], "describe");
    // Keep both stdin and the accepted peer open while the existing deadline fires.
    let failure = bridge.observe(OBSERVATION_TIMEOUT).await.unwrap_err();
    assert!(
        failure.contains("EOF before response waiting for bridge.ready"),
        "{failure}"
    );
    assert!(
        failure.contains("TCP observer action: skipped (port does not establish ownership)"),
        "{failure}"
    );
    let status = tokio::time::timeout(OBSERVATION_TIMEOUT, bridge.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code(), Some(1));
    assert_eq!(bridge.stderr().await, "echo-bridge: deadline\n");
    let rows = trace(&path);
    assert_eq!(rows.last().unwrap()["reason"], "deadline");
    assert_eq!(rows.iter().filter(|row| row["event"] == "wyoming.connected").count(), 0);
    assert_eq!(rows.iter().filter(|row| row["event"] == "run.started").count(), 0);
    drop(stream);
    // This test owns the listener directly; dropping it releases that owner.
    drop(listener);
    bridge.assert_collected();
    fake.shutdown().await;
}

#[tokio::test]
#[ignore = "T3 consumes the prebuilt pin through Python lifecycle regressions"]
async fn pinned_host_startup_coordination_and_failure_regressions() {
    let result = bridge_refusal::host_script("crates/echo-bridge/tests/bridge_observer/startup.sh")
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(result.status.success(), "{}", diagnostics::safe(&result.stderr));
    assert_eq!(result.stdout, [] as [u8; 0]);
    assert!(String::from_utf8(result.stderr).unwrap().trim_end().ends_with("OK"));
}
