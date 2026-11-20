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
    presentation(lines).await["state"].clone()
}

/// Decode the HTTP body before reading complete screen events. HTTP chunk
/// boundaries need not coincide with SSE record boundaries.
pub(super) struct ScreenEvents {
    response: reqwest::Response,
    pending: Vec<u8>,
}

impl ScreenEvents {
    pub(super) async fn connect(address: std::net::SocketAddr) -> Self {
        let response = reqwest::Client::builder()
            .no_proxy()
            .timeout(OBSERVATION_TIMEOUT)
            .build()
            .unwrap()
            .get(format!("http://{address}/events?token=synthetic-screen-pair"))
            .send()
            .await
            .expect("connect to paired screen");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.headers()[reqwest::header::CONTENT_TYPE], "text/event-stream");
        Self {
            response,
            pending: Vec::new(),
        }
    }

    pub(super) async fn next(&mut self, phase: &str) -> Option<Value> {
        loop {
            if let Some(end) = self.pending.windows(2).position(|bytes| bytes == b"\n\n") {
                let data = self.pending[..end].strip_prefix(b"data: ").expect("screen event data");
                let frame = serde_json::from_slice(data).expect("screen event JSON");
                self.pending.drain(..end + 2);
                return Some(frame);
            }
            match self.response.chunk().await {
                Ok(Some(chunk)) => self.pending.extend_from_slice(&chunk),
                Ok(None) => {
                    assert!(self.pending.is_empty(), "{phase}: HTTP body ended inside an SSE record");
                    return None;
                }
                Err(error) => panic!("{phase}: HTTP body read failed: {error:?}"),
            }
        }
    }
}

async fn presentation(lines: &mut Lines<BufReader<TcpStream>>) -> Value {
    tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        loop {
            let line = lines.next_line().await.unwrap().unwrap();
            if let Some(data) = line.strip_prefix("data: ") {
                return serde_json::from_str::<Value>(data).unwrap();
            }
        }
    })
    .await
    .unwrap()
}

async fn assert_page_auth(address: &str) {
    let page = health(address, "GET", "/").await;
    assert!(page.starts_with("HTTP/1.1 200 OK\r\n") && page.contains("<title>Echo screen</title>"));
    for url in ["/events", "/events?token=wrong"] {
        let response = health(address, "GET", url).await;
        assert!(response.starts_with("HTTP/1.1 401 Unauthorized\r\n"));
        assert_eq!(response.split_once("\r\n\r\n").unwrap().1, "");
    }
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
    assert_eq!(
        presentation(&mut events).await,
        json!({"seq":1,"state":{"state":"ready","message":"Echo Live ready"}})
    );
    assert_page_auth(&address).await;
    bridge_short::send(
        &mut satellite,
        "run-pipeline",
        json!({"start_stage":"asr", "end_stage":"tts", "restart_on_end":false,"wake_word_name":"alexa"}),
    )
    .await;
    assert_eq!(state(&mut events).await, json!({"state":"listening"}));
    waiting.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::ZERO, presentation(&mut events))
            .await
            .is_err()
    );
    release.send(()).unwrap();
    assert_eq!(state(&mut events).await, json!({"state":"connected"}));
    assert_eq!(bridge_short::read_reply(&mut satellite).await.0["type"], "audio-start");
    bridge_short::receive_short(&mut satellite, &live, &path, 3).await;
    assert_eq!(state(&mut events).await, json!({"state":"speaking"}));
    bridge_short::send(&mut satellite, "played", json!({})).await;
    assert_eq!(
        serde_json::from_str::<Value>(&bridge.line().await).unwrap(),
        json!({"event":"bridge.run_completed","run_id":"run-1"})
    );
    assert_eq!(
        state(&mut events).await,
        json!({"state":"ready","message":"Echo Live ready"})
    );
    bridge.finish(0).await;
    bridge.assert_collected();
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

// T3: the public SSE stream is the consumption barrier; no sleeps or real account.
#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_mute_snapshot_idle_stray_frames_and_unmute_are_measured() {
    for initially_muted in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        setup(directory.path(), listener.local_addr().unwrap().port());
        let mut bridge = Process::spawn(&mut run_command(directory.path(), "http://127.0.0.1:1"));
        let mut satellite = listener.accept().await.unwrap().0;
        assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "run-satellite");
        assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "describe");
        if initially_muted {
            bridge_short::send(&mut satellite, "hassmic-mute", json!({"muted":true})).await;
        }
        bridge_short::send(&mut satellite, "info", json!({})).await;
        assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
        let address = trace(&path).iter().find(|e| e["event"] == "screen.listening").unwrap()["address"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut events = stream(&address).await;
        let initial = if initially_muted {
            json!({"state":"muted"})
        } else {
            json!({"state":"ready","message":"Echo Live ready"})
        };
        assert_eq!(presentation(&mut events).await, json!({"seq":1,"state":initial}));
        if !initially_muted {
            bridge_short::send(&mut satellite, "hassmic-mute", json!({"muted":true})).await;
            assert_eq!(state(&mut events).await, json!({"state":"muted"}));
        }
        bridge_short::send(&mut satellite, "hassmic-mute", json!({"muted":true})).await;
        microphone_chunk(&mut satellite).await;
        bridge_short::send(&mut satellite, "hassmic-mute", json!({"muted":false})).await;
        assert_eq!(
            state(&mut events).await,
            json!({"state":"ready","message":"Echo Live ready"})
        );
        let windows: Vec<_> = trace(&path)
            .into_iter()
            .filter(|e| e["event"] == "mic.mute_window")
            .collect();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0]["window_id"], "mute-1");
        // Suppressed wire chunks are measured without exposing their audio.
        assert_eq!(windows[0]["received_frames"], 1);
        assert_eq!(windows[0]["forwarded_frames"], 0);
        assert_eq!(trace(&path).iter().filter(|e| e["event"] == "run.started").count(), 0);
        bridge.finish(0).await;
    }
}

pub(super) async fn listening_then_connected(events: &mut Lines<BufReader<TcpStream>>) {
    assert_eq!(state(events).await, json!({"state":"listening"}));
    assert_eq!(state(events).await, json!({"state":"connected"}));
}

pub(super) async fn microphone_chunk(satellite: &mut TcpStream) {
    use echo_contracts::wyoming::{Event, Header, encode};
    satellite
        .write_all(&encode(&Event {
            header: Header {
                r#type: "audio-chunk".into(),
                data: Some(json!({"rate":16000,"width":2,"channels":1})),
                data_length: 0,
                payload_length: 640,
            },
            data: None,
            payload: vec![0; 640],
        }))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_mute_active_and_played_wait_never_forward_or_replace_muted() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    setup(directory.path(), listener.local_addr().unwrap().port());
    let live = bridge_peer::Live::bind().await;
    let mut bridge = Process::spawn(&mut run_command(directory.path(), &live.url));
    let mut satellite = listener.accept().await.unwrap().0;
    assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "run-satellite");
    assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "describe");
    bridge_short::send(&mut satellite, "info", json!({})).await;
    assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
    let address = trace(&path).iter().find(|e| e["event"] == "screen.listening").unwrap()["address"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut events = stream(&address).await;
    assert_eq!(
        state(&mut events).await,
        json!({"state":"ready","message":"Echo Live ready"})
    );
    bridge_short::send(
        &mut satellite,
        "run-pipeline",
        json!({"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"synthetic"}),
    )
    .await;
    assert_eq!(state(&mut events).await, json!({"state":"listening"}));
    assert_eq!(bridge_short::read_reply(&mut satellite).await.0["type"], "audio-start");
    bridge_short::send(&mut satellite, "hassmic-mute", json!({"muted":true})).await;
    // Speaking may have already been published, but can never follow observed Muted.
    loop {
        if state(&mut events).await == json!({"state":"muted"}) {
            break;
        }
    }
    microphone_chunk(&mut satellite).await;
    bridge_short::receive_short(&mut satellite, &live, &path, 3).await;
    // Second chunk arrives while the owner is waiting for Played, rather than producing.
    microphone_chunk(&mut satellite).await;
    bridge_short::send(&mut satellite, "played", json!({})).await;
    assert_eq!(
        serde_json::from_str::<Value>(&bridge.line().await).unwrap(),
        json!({"event":"bridge.run_completed","run_id":"run-1"})
    );
    bridge_short::send(&mut satellite, "hassmic-mute", json!({"muted":false})).await;
    assert_eq!(
        state(&mut events).await,
        json!({"state":"ready","message":"Echo Live ready"})
    );
    assert_muted_run(&path, &live);
    bridge.finish(0).await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_mute_reconnect_resets_absent_snapshot_and_retains_muted_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    setup(directory.path(), listener.local_addr().unwrap().port());
    let mut bridge = Process::spawn(&mut run_command(directory.path(), "http://127.0.0.1:1"));
    let pid = bridge.child.id();
    let mut satellite = listener.accept().await.unwrap().0;
    handshake(&mut satellite, true).await;
    assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
    let address = trace(&path).iter().find(|e| e["event"] == "screen.listening").unwrap()["address"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut events = stream(&address).await;
    assert_eq!(
        presentation(&mut events).await,
        json!({"seq":1,"state":{"state":"muted"}})
    );
    // A new socket with a muted snapshot must not transiently publish Ready.
    drop(satellite);
    satellite = listener.accept().await.unwrap().0;
    reconnect_handshake(&mut satellite, true).await;
    bridge_short::send(&mut satellite, "hassmic-mute", json!({"muted":false})).await;
    assert_eq!(
        presentation(&mut events).await,
        json!({"seq":2,"state":{"state":"ready","message":"Echo Live ready"}})
    );
    bridge_short::send(&mut satellite, "hassmic-mute", json!({"muted":true})).await;
    assert_eq!(
        presentation(&mut events).await,
        json!({"seq":3,"state":{"state":"muted"}})
    );
    // Absence of a snapshot on the next generation is explicitly unmuted.
    drop(satellite);
    satellite = listener.accept().await.unwrap().0;
    reconnect_handshake(&mut satellite, false).await;
    assert_eq!(
        presentation(&mut events).await,
        json!({"seq":4,"state":{"state":"ready","message":"Echo Live ready"}})
    );
    assert_eq!(bridge.child.id(), pid);
    assert_eq!(
        trace(&path).iter().filter(|e| e["event"] == "mic.mute_window").count(),
        2
    );
    bridge.finish(0).await;
}

async fn handshake(satellite: &mut TcpStream, muted: bool) {
    assert_eq!(bridge_short::read(satellite).await.0["type"], "run-satellite");
    reconnect_handshake(satellite, muted).await;
}
async fn reconnect_handshake(satellite: &mut TcpStream, muted: bool) {
    if muted {
        bridge_short::send(satellite, "hassmic-mute", json!({"muted":true})).await;
    }
    // Recovery re-registers before the idle owner probes.
    loop {
        let kind = bridge_short::read(satellite).await.0["type"]
            .as_str()
            .unwrap()
            .to_owned();
        if kind == "describe" {
            break;
        }
        assert_eq!(kind, "run-satellite");
    }
    bridge_short::send(satellite, "info", json!({})).await;
}

fn assert_muted_run(path: &Path, live: &bridge_peer::Live) {
    let entries = trace(path);
    let window = entries.iter().find(|e| e["event"] == "mic.mute_window").unwrap();
    assert_eq!(window["received_frames"], 2);
    assert_eq!(
        entries.iter().find(|e| e["event"] == "mic.completed").unwrap()["dropped_chunks"],
        2
    );
    assert_eq!(window["forwarded_frames"], 0);
    assert_eq!(
        entries.iter().find(|e| e["event"] == "mic.completed").unwrap()["count"],
        0
    );
    assert_eq!(live.microphone_frames.load(std::sync::atomic::Ordering::SeqCst), 0);
}
