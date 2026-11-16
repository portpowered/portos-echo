//! Component T3: the prebuilt executable's public health/control/startup edges.
use super::*;

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
        json!({"status":"running","wyoming":"connected","live":"connecting"})
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
