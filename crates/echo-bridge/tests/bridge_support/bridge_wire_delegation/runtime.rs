//! Component T3 functional proof through the production application host boundary.
//! Uses controlled Tokio deadlines and UTC, not a prebuilt-artifact integration claim.
use super::*;
use echo_bridge::{bridge_input, bridge_runtime};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::oneshot;

struct Application {
    directory: tempfile::TempDir,
    stop: oneshot::Sender<bool>,
    worker: tokio::task::JoinHandle<Result<(), echo_bridge::Error>>,
    satellite: TcpStream,
    day: Arc<AtomicU64>,
    spoof: Spoof,
    model: TcpListener,
    live: bridge_peer::Live,
}

impl Application {
    async fn start(cap: u64, deadline: u64) -> Self {
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
        let live = bridge_peer::Live::bind_frames(0).await;
        let mut settings = settings(directory.path(), &live.url, model.local_addr().unwrap().port());
        let agent = &mut settings.delegation.as_mut().unwrap().0;
        agent.daily_request_cap = cap;
        agent.task_deadline_seconds = deadline;
        let day = Arc::new(AtomicU64::new(17));
        let source = day.clone();
        let (stop, shutdown) = oneshot::channel();
        let worker = tokio::spawn(bridge_runtime::execute_controlled(
            settings,
            bridge_runtime::Control {
                utc_day: Arc::new(move || source.load(Ordering::SeqCst)),
                shutdown,
            },
        ));
        let mut satellite = listener.accept().await.unwrap().0;
        assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "run-satellite");
        assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "describe");
        bridge_short::send(&mut satellite, "info", json!({"satellite":{"name":"synthetic"}})).await;
        let mut application = Self {
            directory,
            stop,
            worker,
            satellite,
            day,
            spoof,
            model,
            live,
        };
        application.wake().await;
        application
    }

    fn path(&self) -> PathBuf {
        self.directory.path().join("trace")
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
        assert_eq!(
            bridge_short::read_reply(&mut self.satellite).await.0["type"],
            "audio-start"
        );
    }

    async fn request(&mut self, id: &str) -> TcpStream {
        self.live.events.send(event(id, "client")).await.unwrap();
        let mut socket = self.model.accept().await.unwrap().0;
        assert_eq!(read(&mut socket).await.body["model"], "fixture-model");
        socket
    }

    async fn commentary(&mut self, id: &str, text: &str) {
        let value = tokio::time::timeout(Duration::from_secs(5), self.live.commentary.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&value).unwrap(),
            json!({
                "type":"session.commentary.append", "delegation_id":id, "text":text
            })
        );
    }

    async fn cancel(&mut self, run: &str) {
        self.live
            .events
            .send(json!({"type":"response.done","response":{"status":"cancelled"}}).to_string())
            .await
            .unwrap();
        assert_eq!(
            bridge_short::read_reply(&mut self.satellite).await.0["type"],
            "audio-stop"
        );
        bridge_short::send(&mut self.satellite, "played", json!({})).await;
        bridge_short::send(&mut self.satellite, "audio-stop", json!({})).await;
        loop {
            if trace(&self.path())
                .iter()
                .any(|e| e["event"] == "reply.played" && e["run_id"] == run)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    }

    async fn finish(self) {
        self.stop.send(true).unwrap();
        assert_eq!(self.worker.await.unwrap(), Ok(()));
        assert_eq!(self.spoof.snapshot().unwrap().calls.len(), 0);
        self.spoof.shutdown().await.unwrap();
    }
}

fn settings(directory: &Path, live: &str, port: u16) -> bridge_input::Settings {
    let mut config = echo_config::BridgeConfig::load(directory.join("bridge.toml")).unwrap();
    config.live.credential_file = directory.join("credential");
    config.screen.pairing_token_file = directory.join("credentials/screen-pairing.txt");
    config.mcp.server[0].bearer_file = Some(directory.join("mcp-token"));
    let providers = echo_signin::resolve_mode(config.live.clone(), config.chatgpt_auth).unwrap();
    let origin = "http://127.0.0.1";
    bridge_input::Settings {
        kill_path: Some(directory.join("audit/bridge-kill")),
        delegation: Some((config.agent, config.mcp.server.remove(0))),
        screen: Some(config.screen),
        wyoming: ([127, 0, 0, 1], config.echo.wyoming_port).into(),
        live_url: live.into(),
        model_url: Some(format!("{origin}:{port}")),
        trace: directory.join("trace"),
        health: ([127, 0, 0, 1], 0).into(),
        mode: echo_live::call::LiveAuthMode::ApiKey,
        voice: "alloy".into(),
        credential: bridge_input::Provider::Resolved(providers),
        lab: false,
        reply_lead_ms: config.audio.reply_lead_ms,
        session_idle_seconds: 120,
    }
}

async fn respond(socket: &mut TcpStream, text: &str) -> std::io::Result<()> {
    let body = sse(&json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}));
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await
}

async fn assert_no_request(listener: &TcpListener) {
    assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(listener.poll_accept(cx).is_pending())).await);
}

#[tokio::test]
#[ignore = "T3 controlled application and loopback peers"]
async fn bridge_wire_runtime_deadline_stops_model_without_tool_or_replay() {
    let mut app = Application::start(3, 1).await;
    let mut stalled = app.request("deadline").await;
    // Keep loopback I/O runnable: paused Tokio must not auto-jump to the
    // outer playback timeout while the OS delivers the deadline commentary.
    let active = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(1)).await;
    // The call owner polls completed workers on its bounded event tick.
    // Advance that tick explicitly while allowing the socket tasks to run.
    for _ in 0..50 {
        tokio::time::advance(Duration::from_millis(20)).await;
        tokio::task::yield_now().await;
        if trace(&app.path())
            .iter()
            .any(|e| e["event"] == "session.commentary.append")
        {
            break;
        }
    }
    tokio::time::resume();
    active.abort();
    app.commentary("deadline", "task timed out").await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), stalled.read(&mut [0]))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert_no_request(&app.model).await;
    assert_eq!(
        trace(&app.path())
            .iter()
            .filter(|e| e["event"] == "session.commentary.append")
            .count(),
        1
    );
    app.finish().await;
}

#[tokio::test]
#[ignore = "T3 controlled application and loopback peers"]
async fn bridge_wire_runtime_abandon_replacement_retains_budget_and_discards_late_result() {
    let mut app = Application::start(1, 60).await;
    let mut old = app.request("old").await;
    app.cancel("run-1").await;
    assert_eq!(old.read(&mut [0]).await.unwrap(), 0);
    app.wake().await;
    let _ = respond(&mut old, "old result must never arrive").await;
    app.live.events.send(event("limited", "client")).await.unwrap();
    app.commentary("limited", "daily request limit reached").await;
    assert_no_request(&app.model).await;
    app.cancel("run-2").await;
    app.day.store(18, Ordering::SeqCst);
    app.wake().await;
    let mut fresh = app.request("fresh").await;
    respond(&mut fresh, "fresh result").await.unwrap();
    app.commentary("fresh", "fresh result").await;
    bridge_wire_auth_failure::wait_event(&app.path(), "session.commentary.append").await;
    let entries = trace(&app.path());
    let appended: Vec<_> = entries
        .iter()
        .filter(|e| e["event"] == "session.commentary.append")
        .map(|e| (e["run_id"].clone(), e["delegation_id"].clone()))
        .collect();
    assert_eq!(
        appended,
        vec![(json!("run-2"), json!("limited")), (json!("run-3"), json!("fresh"))]
    );
    assert_eq!(app.live.calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert_no_request(&app.model).await;
    app.finish().await;
}

#[tokio::test]
#[ignore = "T3 controlled application and loopback peers"]
async fn bridge_wire_runtime_failed_commentary_append_cuts_and_closes_without_success() {
    let mut app = Application::start(3, 60).await;
    let mut request = app.request("oversize").await;
    // Actual LiveCall append rejects its oversized wire frame. Preparation is not delivery.
    respond(&mut request, &"private provider text".repeat(900))
        .await
        .unwrap();
    assert_eq!(
        bridge_short::read_reply(&mut app.satellite).await.0["type"],
        "audio-stop"
    );
    bridge_short::send(&mut app.satellite, "played", json!({})).await;
    bridge_short::send(&mut app.satellite, "audio-stop", json!({})).await;
    bridge_wire_auth_failure::wait_event(&app.path(), "run.failed").await;
    bridge_wire_auth_failure::assert_surfaces(&app.path(), false).await;
    let entries = trace(&app.path());
    for event in [
        "session.commentary.append",
        "reply.played",
        "reply.started",
        "auth.relink_required",
    ] {
        assert_eq!(entries.iter().filter(|e| e["event"] == event).count(), 0);
    }
    assert_eq!(entries.iter().filter(|e| e["event"] == "live.closed").count(), 1);
    assert!(
        !std::fs::read_to_string(app.path())
            .unwrap()
            .contains("private provider text")
    );
    assert!(app.live.commentary.try_recv().is_err());
    assert_no_request(&app.model).await;
    app.wake().await;
    let mut recovered = app.request("recovered").await;
    respond(&mut recovered, "fresh result").await.unwrap();
    app.commentary("recovered", "fresh result").await;
    assert_eq!(app.live.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    app.finish().await;
}
