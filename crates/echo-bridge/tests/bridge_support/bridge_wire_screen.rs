//! Component T3: prebuilt configured binary and real screen library on loopback.
use super::*;
use tokio::net::{TcpListener, TcpStream};

pub(super) fn setup(directory: &Path, port: u16) {
    let path = bridge_wire_auth::config(directory, "api_key", "");
    let content = std::fs::read_to_string(&path)
        .unwrap()
        .replace("wyoming_port = 16700", &format!("wyoming_port = {port}"));
    std::fs::write(path, content).unwrap();
    std::fs::write(directory.join("credential"), "synthetic-api-key\n").unwrap();
    std::fs::create_dir(directory.join("credentials")).unwrap();
    std::fs::write(
        directory.join("credentials/screen-pairing.txt"),
        "synthetic-screen-pair\r\n",
    )
    .unwrap();
}

pub(super) fn run_command(directory: &Path, origin: &str) -> Command {
    bridge_wire_auth::command(
        directory,
        &[
            "--config",
            "bridge.toml",
            "--trace",
            "trace",
            "--health-listen",
            "127.0.0.1:0",
            "--live-loopback",
            origin,
            "--model-loopback",
            origin,
        ],
    )
}

pub(super) async fn stream(address: &str) -> Lines<BufReader<TcpStream>> {
    let mut socket = TcpStream::connect(address).await.unwrap();
    socket
        .write_all(b"GET /events?token=synthetic-screen-pair HTTP/1.1\r\nHost: local\r\n\r\n")
        .await
        .unwrap();
    let mut lines = BufReader::new(socket).lines();
    assert_eq!(lines.next_line().await.unwrap().unwrap(), "HTTP/1.1 200 OK");
    lines
}

pub(super) async fn state(lines: &mut Lines<BufReader<TcpStream>>) -> Value {
    tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        loop {
            let line = lines.next_line().await.unwrap().unwrap();
            if let Some(data) = line.strip_prefix("data: ") {
                return serde_json::from_str::<Value>(data).unwrap()["state"].clone();
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_screen_activity_auth_and_eof_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    setup(directory.path(), listener.local_addr().unwrap().port());
    let (ready, waiting) = tokio::sync::oneshot::channel();
    let (release, gate) = tokio::sync::oneshot::channel();
    let live = bridge_peer::Live::bind_after(3, Some((ready, gate))).await;
    let mut bridge = Process::spawn(&mut run_command(directory.path(), &live.url));
    let mut satellite = listener.accept().await.unwrap().0;
    assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "run-satellite");
    assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "describe");
    bridge_short::send(&mut satellite, "info", json!({"satellite":{"name":"synthetic"}})).await;
    let mut satellite = bridge_short::Satellite::new(satellite);
    assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
    let path = directory.path().join("trace");
    let address = trace(&path).iter().find(|e| e["event"] == "screen.listening").unwrap()["address"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut events = stream(&address).await;
    assert_eq!(state(&mut events).await, json!({"state":"ready","message":"Ready"}));
    let page = health(&address, "GET", "/").await;
    assert!(page.starts_with("HTTP/1.1 200 OK\r\n") && page.contains("<title>Echo screen</title>"));
    for url in ["/events", "/events?token=wrong"] {
        let response = health(&address, "GET", url).await;
        assert!(response.starts_with("HTTP/1.1 401 Unauthorized\r\n"));
        assert_eq!(response.split_once("\r\n\r\n").unwrap().1, "");
    }
    bridge_short::send(
        &mut satellite,
        "run-pipeline",
        json!({"start_stage":"asr", "end_stage":"tts", "restart_on_end":false,"wake_word_name":"alexa"}),
    )
    .await;
    assert_eq!(state(&mut events).await, json!({"state":"listening"}));
    waiting.await.unwrap();
    release.send(()).unwrap();
    assert_eq!(bridge_short::read_reply(&mut satellite).await.0["type"], "audio-start");
    bridge_short::receive_short(&mut satellite, &live, &path, 3).await;
    assert_eq!(state(&mut events).await, json!({"state":"speaking"}));
    bridge_short::send(&mut satellite, "played", json!({})).await;
    assert_eq!(
        serde_json::from_str::<Value>(&bridge.line().await).unwrap(),
        json!({"event":"bridge.run_completed","run_id":"run-1"})
    );
    assert_eq!(state(&mut events).await, json!({"state":"ready","message":"Ready"}));
    bridge.finish(0).await;
    assert!(TcpStream::connect(&address).await.is_err());
    let text = std::fs::read_to_string(path).unwrap();
    assert!(
        !text.contains("synthetic-api-key") && !text.contains("synthetic-screen-pair"),
        "secret disclosure"
    );
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_screen_startup_failure_never_readies_or_dials() {
    for token in [None, Some(""), Some("bad\nvalue"), Some("synthetic-screen-pair")] {
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        setup(directory.path(), listener.local_addr().unwrap().port());
        let path = directory.path().join("credentials/screen-pairing.txt");
        if let Some(token) = token {
            std::fs::write(path, token).unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
        // The valid-token case collides with an already owned listener.
        if token == Some("synthetic-screen-pair") {
            let config = directory.path().join("bridge.toml");
            let text = std::fs::read_to_string(&config)
                .unwrap()
                .replace("127.0.0.1:0", &listener.local_addr().unwrap().to_string());
            std::fs::write(config, text).unwrap();
        }
        let output = run_command(directory.path(), "http://127.0.0.1:1")
            .output()
            .await
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout, b"");
        assert_eq!(output.stderr, b"echo-bridge: screen\n");
        assert_eq!(trace(&directory.path().join("trace")), Vec::<Value>::new());
        // No connection is queued: poll accept once without waiting or a sleep.
        assert!(tokio::time::timeout(Duration::ZERO, listener.accept()).await.is_err());
    }
}
