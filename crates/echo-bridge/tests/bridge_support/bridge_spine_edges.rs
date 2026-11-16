//! Component T3: the prebuilt executable's public health/control/startup edges.
use super::*;

// Read a satellite socket to its close. When the bridge closes with peer
// bytes still unread (a late frame racing stdin EOF), Linux answers with RST
// instead of FIN; both forms are a clean close. Returns the bytes received.
pub(super) async fn read_to_close(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Err(error) = stream.read_to_end(&mut bytes).await {
        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset, "{error}");
    }
    bytes
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_lifecycle_eof_with_idle_probe_response_closes_without_false_capacity_failure() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let mut process = Process::spawn(&mut bridge(&port, "http://127.0.0.1:1", &path));
    let (mut satellite, _) = listener.accept().await.unwrap();
    assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "run-satellite");
    assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "describe");
    bridge_short::send(&mut satellite, "info", json!({})).await;
    assert_eq!(process.line().await, r#"{"event":"bridge.ready"}"#);
    // Synchronize on an outstanding idle probe, then race its response with
    // EOF. Neither a real provider nor a wake is needed for this lifecycle edge.
    assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "describe");
    drop(process.input.take());
    bridge_short::send(&mut satellite, "info", json!({})).await;
    process.finish(0).await;
    let entries = trace(&path);
    assert_eq!(entries.iter().filter(|e| e["event"] == "bridge.error").count(), 0);
    assert_eq!(entries.iter().filter(|e| e["event"] == "run.started").count(), 0);
    let address = entries[0]["health_addr"].as_str().unwrap();
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
    // The late info can still be unread when the bridge closes.
    assert_eq!(read_to_close(&mut satellite).await, Vec::<u8>::new());
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_spine_eof_cancels_registration_before_describe_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let mut process = Process::spawn(&mut bridge(&port, "http://127.0.0.1:1", &path));
    let (mut satellite, _) = listener.accept().await.unwrap();
    assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "run-satellite");
    assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "describe");
    // Withhold info: EOF must cancel the pending registration barrier.
    tokio::time::timeout(Duration::from_secs(2), process.finish(0))
        .await
        .unwrap();
    assert_eq!(process.lines.next_line().await.unwrap(), None);
    let mut bytes = Vec::new();
    satellite.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(bytes, Vec::<u8>::new());
    let entries = trace(&path);
    assert_eq!(
        entries.iter().map(|e| e["event"].as_str().unwrap()).collect::<Vec<_>>(),
        ["bridge.started"]
    );
    let address = entries[0]["health_addr"].as_str().unwrap();
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_spine_eof_during_reply_reaps_satellite_and_health() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let live = bridge_peer::Live::bind().await;
    let (mut satellite, mut process) = bridge_short::satellite(&live.url, &path).await;
    assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "audio-start");
    assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "audio-chunk");
    process.finish(0).await;
    assert_eq!(process.lines.next_line().await.unwrap(), None);
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), satellite.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let entries = trace(&path);
    assert_eq!(entries.iter().filter(|e| e["event"] == "reply.played").count(), 0);
    assert_eq!(entries.iter().filter(|e| e["event"] == "wyoming.run_ended").count(), 0);
    let address = entries[0]["health_addr"].as_str().unwrap();
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_spine_unsupported_microphone_is_safe_and_never_completed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let fake = FakeLive::bind(FakeScript {
        events: vec![],
        response: FakeResponse::Answer,
    })
    .await
    .unwrap();
    let (mut satellite, mut process) = bridge_short::satellite(&fake.base_url(), &path).await;
    bridge_short::send(
        &mut satellite,
        "audio-chunk",
        json!({"rate":44100,"width":2,"channels":1}),
    )
    .await;
    let status = tokio::time::timeout(Duration::from_secs(8), process.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code(), Some(1));
    let entries = trace(&path);
    assert_eq!(entries.last().unwrap()["reason"], "audio");
    assert_eq!(entries.iter().filter(|e| e["event"] == "reply.played").count(), 0);
    assert_eq!(process.lines.next_line().await.unwrap(), None);
    let mut error = String::new();
    process
        .child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut error)
        .await
        .unwrap();
    assert_eq!(error.trim(), "echo-bridge: audio");
    fake.shutdown().await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_spine_trace_open_failure_is_offline() {
    let dir = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let mut process = Process::spawn(&mut bridge(&port, "http://127.0.0.1:1", dir.path()));
    process.finish(1).await;
    assert_eq!(process.lines.next_line().await.unwrap(), None);
    // No connection was queued before the trace failure.
    assert!(tokio::time::timeout(Duration::ZERO, listener.accept()).await.is_err());
}

#[tokio::test]
#[ignore = "T3 requires make hassmic-host and prebuilt echo-bridge"]
async fn bridge_spine_partial_headers_are_bounded_and_eof_cancels_call_setup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let fake = FakeLive::bind(FakeScript {
        events: vec![],
        response: FakeResponse::StallHttp,
    })
    .await
    .unwrap();
    let (_satellite, mut bridge) = bridge_short::satellite(&fake.base_url(), &path).await;
    let entries = trace(&path);
    let address = entries[0]["health_addr"].as_str().unwrap();
    for request in [b"GET /healthz HTTP/1.1\r\n".to_vec(), vec![b'x'; 4096]] {
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream.write_all(&request).await.unwrap();
        stream.shutdown().await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        assert_eq!(response, Vec::<u8>::new());
    }
    tokio::time::timeout(Duration::from_secs(5), fake.observer().wait_for_calls(1))
        .await
        .unwrap()
        .unwrap();
    let response = health(address, "GET", "/healthz").await;
    assert_eq!(
        serde_json::from_str::<Value>(response.split_once("\r\n\r\n").unwrap().1).unwrap(),
        json!({"status":"running","wyoming":"connected","live":"connecting","relink_required":false,"kill_switch":false})
    );
    bridge.finish(0).await;
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
    assert_eq!(trace(&path).iter().filter(|e| e["event"] == "reply.played").count(), 0);
    assert_eq!(fake.observer().calls().unwrap().len(), 1);
    fake.shutdown().await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_spine_occupied_health_listener_prevents_readiness() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut command = bridge("1", "http://127.0.0.1:1", &path);
    command.args(["--health-listen", &listener.local_addr().unwrap().to_string()]);
    let mut process = Process::spawn(&mut command);
    process.finish(1).await;
    assert_eq!(process.lines.next_line().await.unwrap(), None);
    assert_eq!(trace(&path), Vec::<Value>::new());
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_spine_invalid_cli_is_offline_and_creates_no_trace() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    for args in [
        vec!["--unknown"],
        vec!["--health-listen", "localhost:1"],
        vec!["--trace", "unused"],
    ] {
        let mut command = bridge("1", "http://127.0.0.1:1", &path);
        command.args(args);
        let mut process = Process::spawn(&mut command);
        process.finish(2).await;
        assert_eq!(process.lines.next_line().await.unwrap(), None);
        assert!(!path.exists());
    }
}
