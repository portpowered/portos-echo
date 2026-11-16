//! T3: separate wakes through the delivered binary and pinned host share one call.
use super::*;

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_lifecycle_idle_provider_failure_next_wake_creates_fresh_call() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let live = bridge_peer::Live::bind().await;
    let (mut satellite, mut process) = bridge_short::satellite(&live.url, &path).await;
    let pid = process.child.id();
    for run in 1..=2 {
        assert_eq!(bridge_short::read_reply(&mut satellite).await.0["type"], "audio-start");
        bridge_short::receive_short(&mut satellite, &live, &path, 3).await;
        bridge_short::send(&mut satellite, "played", json!({})).await;
        assert_eq!(
            serde_json::from_str::<Value>(&process.line().await).unwrap(),
            json!({"event":"bridge.run_completed","run_id":format!("run-{run}")})
        );
        assert_eq!(process.child.id(), pid);
        assert_eq!(live.calls.load(std::sync::atomic::Ordering::SeqCst), run);
        if run == 1 {
            live.events.send(json!({"type":"error"}).to_string()).await.unwrap();
            idle_failure(&path).await;
            bridge_short::send(
                &mut satellite,
                "run-pipeline",
                json!({"start_stage":"asr", "end_stage":"tts", "restart_on_end":false, "wake_word_name":"alexa"}),
            )
            .await;
        }
    }
    let entries = trace(&path);
    assert_eq!(entries.iter().filter(|e| e["event"] == "reply.played").count(), 2);
    assert_eq!(entries.iter().filter(|e| e["event"] == "bridge.error").count(), 0);
    let closes: Vec<_> = entries.iter().filter(|e| e["event"] == "live.closed").collect();
    assert_eq!(closes.len(), 1);
    assert_eq!(closes[0]["reason"], "unavailable");
    process.finish(0).await;
    let address = entries[0]["health_addr"].as_str().unwrap();
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
}

async fn idle_failure(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let entries = trace(path);
            if entries.iter().any(|e| e["event"] == "live.closed") {
                let address = entries[0]["health_addr"].as_str().unwrap();
                let response = health(address, "GET", "/healthz").await;
                assert_eq!(
                    serde_json::from_str::<Value>(response.split_once("\r\n\r\n").unwrap().1).unwrap(),
                    json!({"status":"running","wyoming":"connected","live":"error","relink_required":false,"kill_switch":false})
                );
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_lifecycle_invalid_idle_override_exits_offline_without_trace() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    for seconds in ["-1", "18446744073709551616", "private-invalid-value"] {
        let output = bridge("1", "http://127.0.0.1:1", &path)
            .env("ECHO_LAB_SESSION_IDLE_SECONDS", seconds)
            .output()
            .await
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(output.stdout, b"");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap().trim(),
            "echo-bridge: invalid input"
        );
        assert!(!path.exists());
    }
}

#[tokio::test]
#[ignore = "T3 requires make hassmic-host and prebuilt echo-bridge"]
async fn bridge_lifecycle_expiry_closes_once_and_next_wake_creates_fresh_call() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let fake = FakeLive::bind(FakeScript {
        events: vec![],
        response: FakeResponse::Answer,
    })
    .await
    .unwrap();
    let mut host = Process::spawn(&mut host());
    let port = host.line().await;
    let mut command = bridge(&port, &fake.base_url(), &path);
    command.env("ECHO_LAB_SESSION_IDLE_SECONDS", "1");
    let mut process = Process::spawn(&mut command);
    let pid = process.child.id();
    assert_eq!(process.line().await, r#"{"event":"bridge.ready"}"#);
    for run in 1..=2 {
        assert_eq!(host.command("wake").await, "wake");
        assert_eq!(
            serde_json::from_str::<Value>(&process.line().await).unwrap(),
            json!({"event":"bridge.run_completed","run_id":format!("run-{run}")})
        );
        assert_eq!(process.child.id(), pid);
        assert_eq!(host.command("pcm").await, format!("{} 0", run * 30720));
        let calls = fake.observer().calls().unwrap();
        assert_eq!(calls.len(), run);
        assert_eq!(calls[run - 1].frames.len(), 32);
        assert_eq!(calls[run - 1].frames[0].sequence, 0);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let entries = trace(&path);
                let closes: Vec<_> = entries.iter().filter(|e| e["event"] == "live.closed").collect();
                if closes.len() >= run {
                    assert_eq!(closes.len(), run);
                    assert!(closes.iter().all(|e| e["reason"] == "idle"));
                    break;
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        })
        .await
        .unwrap();
    }
    let entries = trace(&path);
    assert_eq!(entries.iter().filter(|e| e["event"] == "reply.played").count(), 2);
    assert_eq!(entries.iter().filter(|e| e["event"] == "bridge.error").count(), 0);
    process.finish(0).await;
    assert_eq!(trace(&path).iter().filter(|e| e["event"] == "live.closed").count(), 2);
    let address = entries[0]["health_addr"].as_str().unwrap();
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
    host.finish(0).await;
    fake.shutdown().await;
}

#[tokio::test]
#[ignore = "T3 requires make hassmic-host and prebuilt echo-bridge"]
async fn bridge_lifecycle_wake_during_playback_stays_in_the_existing_run() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let live = bridge_peer::Live::bind_frames(150).await;
    let mut host = Process::spawn(&mut host());
    let port = host.line().await;
    let mut process = Process::spawn(&mut bridge(&port, &live.url, &path));
    assert_eq!(process.line().await, r#"{"event":"bridge.ready"}"#);
    assert_eq!(host.command("arm").await, "armed");
    assert_eq!(host.command("wake").await, "wake");
    assert_eq!(host.command("cut-ready").await, "cut-ready");
    assert_eq!(host.command("wake-playing").await, "wake-playing");
    live.events
        .send(json!({"type":"response.done","response":{"status":"cancelled"}}).to_string())
        .await
        .unwrap();
    let latency: f64 = host.command("cut-result").await.parse().unwrap();
    assert!((0.0..600.0).contains(&latency));
    assert_eq!(
        serde_json::from_str::<Value>(&process.line().await).unwrap(),
        json!({"event":"bridge.run_completed","run_id":"run-1"})
    );
    let entries = trace(&path);
    for name in [
        "run.started",
        "reply.started",
        "reply.cut",
        "reply.played",
        "mic.completed",
        "wyoming.run_ended",
    ] {
        let records: Vec<_> = entries.iter().filter(|e| e["event"] == name).collect();
        assert_eq!(records.len(), 1, "{name}");
        assert_eq!(records[0]["run_id"], "run-1");
    }
    process.finish(0).await;
    host.finish(0).await;
}

async fn link(address: &str, expected: &str) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let response = health(address, "GET", "/healthz").await;
            let body: Value = serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(body["status"], "running");
            if body["wyoming"] == expected {
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .unwrap();
}

// T3: reproduce the hassmic pipeline-timeout close without restarting either
// process. The listener is withheld until health observes the outage, so a fast
// reconnect cannot hide the disconnected state. All endpoints are loopback.
#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_lifecycle_idle_socket_close_after_credential_refusal_recovers() {
    idle_socket_close(false).await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_lifecycle_idle_socket_close_after_streamed_wake_recovers() {
    idle_socket_close(true).await;
}

async fn idle_socket_close(authorized: bool) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap();
    bridge_wire_screen::setup(dir.path(), endpoint.port());
    let config = dir.path().join("bridge.toml");
    let contents = std::fs::read_to_string(&config)
        .unwrap()
        .replace("session_idle_seconds = 120", "session_idle_seconds = 0");
    std::fs::write(config, contents).unwrap();
    if !authorized {
        std::fs::remove_file(dir.path().join("credential")).unwrap();
    }
    let live = bridge_peer::Live::bind().await;
    let mut process = Process::spawn(&mut bridge_wire_screen::run_command(dir.path(), &live.url));
    let pid = process.child.id();
    let mut satellite = listener.accept().await.unwrap().0;
    bridge_short::register(&mut satellite, &mut process).await;
    idle_turn(&mut satellite, &mut process, &live, &path, 1, authorized).await;
    let address = trace(&path)[0]["health_addr"].as_str().unwrap().to_owned();
    let mut satellite = recovered_socket(listener, satellite, &address).await;
    bridge_short::send(
        &mut satellite,
        "run-pipeline",
        json!({"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"alexa"}),
    )
    .await;
    idle_turn(&mut satellite, &mut process, &live, &path, 2, authorized).await;
    assert_eq!(process.child.id(), pid);
    process.finish(0).await;
    assert_idle_recovery(&path, authorized);
    assert_idle_cleanup(satellite, &path, &address).await;
}

async fn recovered_socket(
    listener: tokio::net::TcpListener,
    satellite: tokio::net::TcpStream,
    address: &str,
) -> tokio::net::TcpStream {
    let endpoint = listener.local_addr().unwrap();
    drop(listener);
    drop(satellite);
    link(address, "reconnecting").await;
    let listener = tokio::net::TcpListener::bind(endpoint).await.unwrap();
    let mut satellite = tokio::time::timeout(Duration::from_secs(8), listener.accept())
        .await
        .unwrap()
        .unwrap()
        .0;
    idle_registration(&mut satellite).await;
    link(address, "connected").await;
    satellite
}

async fn assert_idle_cleanup(mut satellite: tokio::net::TcpStream, path: &Path, address: &str) {
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
    let screen = trace(path)
        .into_iter()
        .find(|e| e["event"] == "screen.listening")
        .unwrap();
    assert!(
        tokio::net::TcpStream::connect(screen["address"].as_str().unwrap())
            .await
            .is_err()
    );
    let mut remaining = Vec::new();
    satellite.read_to_end(&mut remaining).await.unwrap();
    // A final idle describe can race stdin EOF; no reply audio may remain.
    for line in remaining.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()) {
        assert_eq!(serde_json::from_slice::<Value>(line).unwrap()["type"], "describe");
    }
}

async fn idle_registration(satellite: &mut tokio::net::TcpStream) {
    assert_eq!(bridge_short::read(satellite).await.0["type"], "run-satellite");
    assert_eq!(bridge_short::read(satellite).await.0["type"], "describe");
    bridge_short::send(satellite, "info", json!({})).await;
}

async fn idle_turn(
    satellite: &mut tokio::net::TcpStream,
    process: &mut Process,
    live: &bridge_peer::Live,
    path: &Path,
    run: u64,
    authorized: bool,
) {
    let id = format!("run-{run}");
    if authorized {
        for _ in 0..10 {
            bridge_wire_screen::microphone_chunk(satellite).await;
        }
        bridge_short::send(satellite, "audio-stop", json!({})).await;
        assert_eq!(bridge_short::read_reply(satellite).await.0["type"], "audio-start");
        bridge_short::receive_short(satellite, live, path, 3).await;
        bridge_short::send(satellite, "played", json!({})).await;
        assert_eq!(
            serde_json::from_str::<Value>(&process.line().await).unwrap(),
            json!({"event":"bridge.run_completed","run_id":id})
        );
    } else {
        let event = if run == 1 {
            "auth.relink_required"
        } else {
            "run.refused"
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if trace(path).iter().any(|e| e["event"] == event && e["run_id"] == id) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(live.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}

fn assert_idle_recovery(path: &Path, authorized: bool) {
    let entries = trace(path);
    let states: Vec<_> = entries
        .iter()
        .filter(|e| e["event"] == "wyoming.connected" || e["event"] == "wyoming.reconnecting")
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        states,
        ["wyoming.connected", "wyoming.reconnecting", "wyoming.connected"]
    );
    assert_eq!(entries.iter().filter(|e| e["event"] == "bridge.error").count(), 0);
    let runs: Vec<_> = entries.iter().filter(|e| e["event"] == "wyoming.run_started").collect();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0]["run_id"], "run-1");
    assert_eq!(runs[1]["run_id"], "run-2");
    let microphone: Vec<_> = entries.iter().filter(|e| e["event"] == "wyoming.mic_frames").collect();
    assert_eq!(microphone.len(), if authorized { 2 } else { 0 });
    for (index, record) in microphone.iter().enumerate() {
        assert_eq!(record["run_id"], format!("run-{}", index + 1));
        assert_eq!(record["count"], 10);
        let started = entries
            .iter()
            .position(|e| e["event"] == "wyoming.run_started" && e["run_id"] == record["run_id"])
            .unwrap();
        let counted = entries.iter().position(|e| e == *record).unwrap();
        let ended = entries
            .iter()
            .position(|e| e["event"] == "wyoming.run_ended" && e["run_id"] == record["run_id"])
            .unwrap();
        assert!(started < counted && counted < ended);
    }
    assert_eq!(
        entries.iter().filter(|e| e["event"] == "reply.played").count(),
        if authorized { 2 } else { 0 }
    );
}

#[tokio::test]
#[ignore = "T3 requires make hassmic-host and prebuilt echo-bridge"]
async fn bridge_lifecycle_idle_host_restart_recovers_without_a_bridge_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let fake = FakeLive::bind(FakeScript {
        events: vec![],
        response: FakeResponse::Answer,
    })
    .await
    .unwrap();
    let mut host = Process::spawn(&mut host());
    let port = host.line().await;
    let mut process = Process::spawn(&mut bridge(&port, &fake.base_url(), &path));
    let pid = process.child.id();
    assert_eq!(process.line().await, r#"{"event":"bridge.ready"}"#);
    assert_eq!(host.command("wake").await, "wake");
    assert_eq!(
        serde_json::from_str::<Value>(&process.line().await).unwrap(),
        json!({"event":"bridge.run_completed","run_id":"run-1"})
    );
    let address = trace(&path)[0]["health_addr"].as_str().unwrap().to_owned();
    assert_eq!(host.command("stop").await, "stop");
    link(&address, "reconnecting").await;
    assert_eq!(host.command("restart").await, "restart");
    link(&address, "connected").await;
    assert_eq!(host.command("wake").await, "wake");
    assert_eq!(
        serde_json::from_str::<Value>(&process.line().await).unwrap(),
        json!({"event":"bridge.run_completed","run_id":"run-2"})
    );
    assert_eq!(host.command("pcm").await, "61440 0");
    assert_eq!(process.child.id(), pid);
    let calls = fake.observer().calls().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].frames.len(), 64);
    let entries = trace(&path);
    let states: Vec<_> = entries
        .iter()
        .filter(|e| e["event"] == "wyoming.connected" || e["event"] == "wyoming.reconnecting")
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        states,
        ["wyoming.connected", "wyoming.reconnecting", "wyoming.connected"]
    );
    assert_eq!(entries.iter().filter(|e| e["event"] == "reply.played").count(), 2);
    process.finish(0).await;
    assert!(tokio::net::TcpStream::connect(&address).await.is_err());
    host.finish(0).await;
    fake.shutdown().await;
}

#[tokio::test]
#[ignore = "T3 requires make hassmic-host and prebuilt echo-bridge"]
async fn bridge_lifecycle_three_wakes_reuse_one_call_with_per_run_audio_counts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let fake = FakeLive::bind(FakeScript {
        events: vec![],
        response: FakeResponse::Answer,
    })
    .await
    .unwrap();
    let observer = fake.observer();
    let mut host = Process::spawn(&mut host());
    let port = host.line().await;
    let mut process = Process::spawn(&mut bridge(&port, &fake.base_url(), &path));
    let pid = process.child.id();
    assert_eq!(process.line().await, r#"{"event":"bridge.ready"}"#);
    for run in 1..=3 {
        assert_eq!(host.command("wake").await, "wake");
        assert_eq!(
            serde_json::from_str::<Value>(&process.line().await).unwrap(),
            json!({"event":"bridge.run_completed","run_id":format!("run-{run}")})
        );
        assert_eq!(process.child.id(), pid);
        assert_eq!(host.command("pcm").await, format!("{} 0", run * 30720));
        let calls = observer.calls().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].frames.len(), run * 32);
        assert_eq!(
            calls[0].frames.iter().map(|f| f.sequence).collect::<Vec<_>>(),
            (0..u16::try_from(run * 32).unwrap()).collect::<Vec<_>>()
        );
        let entries = trace(&path);
        let mic: Vec<_> = entries.iter().filter(|e| e["event"] == "mic.completed").collect();
        assert_eq!(mic.len(), run);
        assert_eq!(mic.last().unwrap()["run_id"], format!("run-{run}"));
        assert_eq!(mic.last().unwrap()["count"], 32);
        let wire = entries
            .iter()
            .rev()
            .find(|e| e["event"] == "wyoming.mic_frames")
            .unwrap();
        assert_eq!(mic.last().unwrap()["received_chunks"], wire["count"]);
        assert!(wire["count"].as_u64().unwrap() >= 10);
        assert_eq!(mic.last().unwrap()["input_samples"], 10240);
        let address = entries[0]["health_addr"].as_str().unwrap();
        assert_health(address).await;
    }
    let entries = trace(&path);
    assert_eq!(entries.iter().filter(|e| e["event"] == "reply.played").count(), 3);
    assert_eq!(entries.iter().filter(|e| e["event"] == "live.closed").count(), 0);
    process.finish(0).await;
    let address = entries[0]["health_addr"].as_str().unwrap();
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
    host.finish(0).await;
    fake.shutdown().await;
}
