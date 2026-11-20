//! Component T3: separate operator process controls the prebuilt running binary.
use super::*;
use std::sync::atomic::Ordering;

struct Rig {
    directory: tempfile::TempDir,
    listener: TcpListener,
    satellite: bridge_short::Satellite,
    bridge: Option<Process>,
    live: bridge_peer::Live,
    model: TcpListener,
    spoof: Spoof,
}

impl Rig {
    async fn start(frames: u16) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let model = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let spoof = Spoof::start(Options::default()).await.unwrap();
        configure(
            directory.path(),
            listener.local_addr().unwrap().port(),
            "api_key",
            &spoof,
            true,
        )
        .await;
        let live = bridge_peer::Live::bind_frames(frames).await;
        let base = "http://127.0.0.1";
        let origin = format!("{base}:{}", model.local_addr().unwrap().port());
        let mut bridge = Process::spawn(&mut run_command(directory.path(), &live.url, &origin));
        let mut satellite = listener.accept().await.unwrap().0;
        register(&mut satellite, &mut bridge).await;
        let satellite = bridge_short::Satellite::new(satellite);
        Self {
            directory,
            listener,
            satellite,
            bridge: Some(bridge),
            live,
            model,
            spoof,
        }
    }
    fn path(&self) -> PathBuf {
        self.directory.path().join("trace")
    }
    async fn command(&self, clear: bool) {
        let mut args = vec!["kill", "--config", "bridge.toml"];
        if clear {
            args.push("--clear");
        }
        bridge_wire_auth::invoke(
            self.directory.path(),
            &args,
            0,
            if clear {
                "kill-switch: off\n"
            } else {
                "kill-switch: on\n"
            },
        )
        .await;
    }
    async fn wake(&mut self) {
        bridge_short::send(
            &mut self.satellite,
            "run-pipeline",
            json!({
                "start_stage":"asr", "end_stage":"tts", "restart_on_end":false, "wake_word_name":"alexa"
            }),
        )
        .await;
    }
    async fn cut(&mut self) -> usize {
        let mut chunks = 0;
        loop {
            let (header, payload) = bridge_short::read_reply(&mut self.satellite).await;
            match header["type"].as_str().unwrap() {
                "audio-chunk" => chunks += 1,
                "hassmic-flush" => {
                    let expected = json!({"type":"hassmic-flush","data":null,"data_length":0,"payload_length":0});
                    assert_eq!((header, payload), (expected, vec![]));
                    break;
                }
                kind => assert_eq!(kind, "audio-chunk"),
            }
        }
        if chunks == 0 {
            assert_kill_error(&mut self.satellite).await;
        }
        bridge_short::send(&mut self.satellite, "played", json!({})).await;
        bridge_short::send(&mut self.satellite, "audio-stop", json!({})).await;
        bridge_wire_auth_failure::wait_event(&self.path(), "live.closed", self.bridge.as_mut().unwrap()).await;
        if chunks > 0 {
            // Settle the cut run's explicit end before a later wake is refused.
            bridge_lifecycle::control(&mut self.satellite, "hassmic-end").await;
        }
        chunks
    }

    async fn refused(&mut self, run: &str) {
        self.wake().await;
        assert_kill_error(&mut self.satellite).await;
        bridge_wire_auth_failure::wait_event(&self.path(), "run.refused", self.bridge.as_mut().unwrap()).await;
        assert!(
            trace(&self.path())
                .iter()
                .any(|e| e["event"] == "run.refused" && e["run_id"] == run && e["reason"] == "kill")
        );
        assert_eq!(self.live.calls.load(Ordering::SeqCst), 1);
        assert_eq!(self.spoof.snapshot().unwrap().calls.len(), 0);
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(self.model.poll_accept(cx).is_pending())).await);
    }

    async fn finish(self) {
        self.bridge.unwrap().finish(0).await;
        self.spoof.shutdown().await.unwrap();
    }
}

async fn assert_kill_error(satellite: &mut bridge_short::Satellite) {
    bridge_lifecycle::control(satellite, "hassmic-end").await;
    let (header, payload) = bridge_short::read_reply(satellite).await;
    assert_eq!(header["type"], "error");
    let expected = json!({"code":"echo-kill-switch","text":"Kill switch enabled."});
    assert_eq!(header["data"], expected);
    assert_eq!(payload, Vec::<u8>::new());
}

async fn register(satellite: &mut TcpStream, bridge: &mut Process) {
    assert_eq!(bridge_short::read(satellite).await.0["type"], "run-satellite");
    assert_eq!(bridge_short::read(satellite).await.0["type"], "describe");
    bridge_short::send(satellite, "info", json!({"satellite":{"name":"synthetic"}})).await;
    assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
}

async fn screen(path: &Path) -> Lines<BufReader<TcpStream>> {
    let address = trace(path).iter().find(|e| e["event"] == "screen.listening").unwrap()["address"]
        .as_str()
        .unwrap()
        .to_owned();
    bridge_wire_screen::stream(&address).await
}

async fn assert_killed(path: &Path) {
    let address = trace(path).iter().find(|e| e["event"] == "bridge.started").unwrap()["health_addr"]
        .as_str()
        .unwrap()
        .to_owned();
    let response = health(&address, "GET", "/healthz").await;
    let body: Value = serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(
        body,
        json!({"status":"running","wyoming":"connected","live":"error",
        "relink_required":false,"kill_switch":true,"mcp":"disabled"})
    );
    let mut events = screen(path).await;
    assert_eq!(
        bridge_wire_screen::state(&mut events).await,
        json!({"state":"error",
        "message":"Kill switch enabled. Run echo-bridge kill --clear with your config.","relink":false})
    );
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_kill_reply_refuses_restart_and_clear_creates_fresh_call() {
    let mut rig = Rig::start(32).await;
    rig.wake().await;
    assert_eq!(
        bridge_short::read_reply(&mut rig.satellite).await.0["type"],
        "audio-start"
    );
    bridge_wire_auth_failure::wait_event(&rig.path(), "reply.started", rig.bridge.as_mut().unwrap()).await;
    rig.command(false).await;
    let chunks = rig.cut().await;
    assert!(chunks < 32, "kill must discard the unsent reply");
    assert_killed(&rig.path()).await;
    rig.command(false).await;
    rig.satellite.responding(false).await;
    rig.refused("run-2").await;
    assert_eq!(bridge_short::read(&mut rig.satellite).await.0["type"], "describe");
    bridge_short::send(&mut rig.satellite, "info", json!({"satellite":{"name":"synthetic"}})).await;
    let entries = trace(&rig.path());
    assert_eq!(entries.iter().filter(|e| e["event"] == "kill.enabled").count(), 1);
    let cut = entries.iter().find(|e| e["event"] == "reply.cut").unwrap();
    assert!(cut["dropped_frames"].as_u64().unwrap() > 0);
    assert!(
        entries
            .iter()
            .any(|e| e["event"] == "live.closed" && e["reason"] == "kill")
    );
    restart_and_clear(&mut rig).await;
    rig.finish().await;
}

async fn restart_and_clear(rig: &mut Rig) {
    // The new process appends to trace; isolate its observations as in the retained restart tests.
    rig.bridge.take().unwrap().finish(0).await;
    std::fs::write(rig.path(), "").unwrap();
    let base = "http://127.0.0.1";
    let origin = format!("{base}:{}", rig.model.local_addr().unwrap().port());
    let mut bridge = Process::spawn(&mut run_command(rig.directory.path(), &rig.live.url, &origin));
    let mut satellite = rig.listener.accept().await.unwrap().0;
    register(&mut satellite, &mut bridge).await;
    rig.satellite = bridge_short::Satellite::new(satellite);
    rig.bridge = Some(bridge);
    bridge_wire_auth_failure::wait_event(&rig.path(), "kill.enabled", rig.bridge.as_mut().unwrap()).await;
    assert_killed(&rig.path()).await;
    rig.refused("run-1").await;
    rig.command(true).await;
    rig.command(true).await;
    bridge_wire_auth_failure::wait_event(&rig.path(), "kill.cleared", rig.bridge.as_mut().unwrap()).await;
    fresh(rig, "run-2").await;
}

async fn fresh(rig: &mut Rig, run: &str) {
    rig.wake().await;
    assert_eq!(
        bridge_short::read_reply(&mut rig.satellite).await.0["type"],
        "audio-start"
    );
    rig.live.events.send(event("fresh", "client")).await.unwrap();
    let mut socket = rig.model.accept().await.unwrap().0;
    assert_eq!(read(&mut socket).await.body["model"], "fixture-model");
    answer(&mut socket, "fresh result").await;
    let appended = rig.live.commentary.recv().await.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&appended).unwrap(),
        json!({
        "type":"session.commentary.append","delegation_id":"fresh","text":"fresh result"})
    );
    rig.live
        .events
        .send(json!({"type":"response.audio.done"}).to_string())
        .await
        .unwrap();
    completed(rig, run).await;
    assert_fresh_effects(rig);
}

fn assert_fresh_effects(rig: &mut Rig) {
    assert_eq!(rig.live.calls.load(Ordering::SeqCst), 2);
    assert!(rig.live.commentary.try_recv().is_err());
    assert_eq!(rig.spoof.snapshot().unwrap().calls.len(), 0);
}

async fn answer(socket: &mut TcpStream, text: &str) {
    let body = sse(&json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}));
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
}

async fn completed(rig: &mut Rig, run: &str) {
    while bridge_short::read_reply(&mut rig.satellite).await.0["type"] != "audio-stop" {}
    bridge_lifecycle::played(&mut rig.satellite).await;
    bridge_short::send(&mut rig.satellite, "audio-stop", json!({})).await;
    assert_eq!(
        serde_json::from_str::<Value>(&rig.bridge.as_mut().unwrap().line().await).unwrap(),
        json!({"event":"bridge.run_completed","run_id":run})
    );
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_kill_abandons_delegation_and_never_replays_after_clear() {
    let mut rig = Rig::start(0).await;
    rig.wake().await;
    assert_eq!(
        bridge_short::read_reply(&mut rig.satellite).await.0["type"],
        "audio-start"
    );
    rig.live.events.send(event("abandoned", "client")).await.unwrap();
    let mut old = rig.model.accept().await.unwrap().0;
    assert_eq!(read(&mut old).await.body["model"], "fixture-model");
    rig.command(false).await;
    assert_eq!(rig.cut().await, 0);
    assert_eq!(old.read(&mut [0]).await.unwrap(), 0);
    assert!(rig.live.commentary.try_recv().is_err());
    rig.refused("run-2").await;
    rig.command(true).await;
    bridge_wire_auth_failure::wait_event(&rig.path(), "kill.cleared", rig.bridge.as_mut().unwrap()).await;
    let _ = old.write_all(b"late abandoned response").await;
    fresh(&mut rig, "run-3").await;
    let appended: Vec<_> = trace(&rig.path())
        .into_iter()
        .filter(|e| e["event"] == "session.commentary.append")
        .map(|e| (e["run_id"].clone(), e["delegation_id"].clone()))
        .collect();
    assert_eq!(appended, vec![(json!("run-3"), json!("fresh"))]);
    rig.finish().await;
}

async fn wake_socket(socket: &mut bridge_short::Satellite) {
    bridge_short::send(
        socket,
        "run-pipeline",
        json!({
            "start_stage":"asr", "end_stage":"tts", "restart_on_end":false, "wake_word_name":"alexa"
        }),
    )
    .await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_kill_during_call_setup_cancels_http_and_refuses_another_request() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let live = TcpListener::bind("127.0.0.1:0").await.unwrap();
    bridge_wire_screen::setup(directory.path(), listener.local_addr().unwrap().port());
    let base = "http://127.0.0.1";
    let origin = format!("{base}:{}", live.local_addr().unwrap().port());
    let mut bridge = Process::spawn(&mut bridge_wire_screen::run_command(directory.path(), &origin));
    let mut satellite = listener.accept().await.unwrap().0;
    register(&mut satellite, &mut bridge).await;
    let mut satellite = bridge_short::Satellite::new(satellite);
    wake_socket(&mut satellite).await;
    let mut pending = live.accept().await.unwrap().0;
    assert_eq!(read(&mut pending).await.body["session"]["model"], "gpt-live-1");
    bridge_wire_auth::invoke(
        directory.path(),
        &["kill", "--config", "bridge.toml"],
        0,
        "kill-switch: on\n",
    )
    .await;
    let path = directory.path().join("trace");
    bridge_wire_auth_failure::wait_event(&path, "kill.enabled", &mut bridge).await;
    assert_eq!(pending.read(&mut [0]).await.unwrap(), 0);
    assert_killed(&path).await;
    assert_kill_error(&mut satellite).await;
    wake_socket(&mut satellite).await;
    assert_kill_error(&mut satellite).await;
    bridge_wire_auth_failure::wait_event(&path, "run.refused", &mut bridge).await;
    assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(live.poll_accept(cx).is_pending())).await);
    assert_eq!(trace(&path).iter().filter(|e| e["event"] == "reply.started").count(), 0);
    bridge_refusal::setup_clear_and_reply(directory.path(), &mut satellite, live, &mut bridge).await;
    bridge.finish(0).await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_kill_invalid_marker_fails_closed_without_success_or_dial() {
    for nonempty in [false, true] {
        invalid_marker(nonempty).await;
    }
}

async fn invalid_marker(nonempty: bool) {
    let directory = tempfile::tempdir().unwrap();
    bridge_wire_screen::setup(directory.path(), 1);
    let marker = directory.path().join("logs/audit/bridge-kill");
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    if nonempty {
        std::fs::write(&marker, "synthetic private marker").unwrap();
    } else {
        std::fs::create_dir(&marker).unwrap();
    }
    for clear in [false, true] {
        let mut args = vec!["kill", "--config", "bridge.toml"];
        if clear {
            args.push("--clear");
        }
        let output = bridge_wire_auth::command(directory.path(), &args)
            .output()
            .await
            .unwrap();
        assert_control(&output);
    }
    let output = bridge_wire_screen::run_command(directory.path(), "http://127.0.0.1:1")
        .output()
        .await
        .unwrap();
    assert_control(&output);
    assert!(!directory.path().join("trace").exists());
    assert_eq!(marker.is_file(), nonempty);
    if nonempty {
        assert_eq!(std::fs::read(&marker).unwrap(), b"synthetic private marker");
    }
}

fn assert_control(output: &std::process::Output) {
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"");
    assert_eq!(output.stderr, b"echo-bridge: control\n");
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_kill_waiting_for_played_abandons_pending_model_before_ack() {
    let mut rig = Rig::start(0).await;
    rig.wake().await;
    assert_eq!(
        bridge_short::read_reply(&mut rig.satellite).await.0["type"],
        "audio-start"
    );
    rig.live.events.send(event("pending", "client")).await.unwrap();
    let mut model = rig.model.accept().await.unwrap().0;
    assert_eq!(read(&mut model).await.body["model"], "fixture-model");
    rig.live
        .events
        .send(json!({"type":"response.audio.done"}).to_string())
        .await
        .unwrap();
    assert_eq!(
        bridge_short::read_reply(&mut rig.satellite).await.0["type"],
        "audio-stop"
    );
    rig.command(false).await;
    bridge_wire_auth_failure::wait_event(&rig.path(), "kill.enabled", rig.bridge.as_mut().unwrap()).await;
    assert_eq!(model.read(&mut [0]).await.unwrap(), 0);
    assert_killed(&rig.path()).await;
    assert_eq!(
        trace(&rig.path())
            .iter()
            .filter(|e| e["event"] == "reply.played")
            .count(),
        0
    );
    bridge_lifecycle::control(&mut rig.satellite, "hassmic-end").await;
    bridge_short::send(&mut rig.satellite, "played", json!({})).await;
    bridge_short::send(&mut rig.satellite, "audio-stop", json!({})).await;
    bridge_wire_auth_failure::wait_event(&rig.path(), "live.closed", rig.bridge.as_mut().unwrap()).await;
    assert_kill_error(&mut rig.satellite).await;
    rig.refused("run-2").await;
    assert!(rig.live.commentary.try_recv().is_err());
    rig.finish().await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_kill_idle_call_closes_once_and_runtime_corruption_refuses_wake() {
    let mut rig = Rig::start(0).await;
    rig.wake().await;
    assert_eq!(
        bridge_short::read_reply(&mut rig.satellite).await.0["type"],
        "audio-start"
    );
    rig.live
        .events
        .send(json!({"type":"response.audio.done"}).to_string())
        .await
        .unwrap();
    completed(&mut rig, "run-1").await;
    // A path that becomes invalid after startup is a kill, never permission to dial.
    let marker = rig.directory.path().join("logs/audit/bridge-kill");
    std::fs::create_dir_all(&marker).unwrap();
    bridge_wire_auth_failure::wait_event(&rig.path(), "live.closed", rig.bridge.as_mut().unwrap()).await;
    assert_killed(&rig.path()).await;
    rig.refused("run-2").await;
    assert_eq!(
        trace(&rig.path())
            .iter()
            .filter(|e| e["event"] == "live.closed")
            .count(),
        1
    );
    assert!(
        trace(&rig.path())
            .iter()
            .any(|e| e["event"] == "live.closed" && e["reason"] == "kill")
    );
    rig.finish().await;
}
