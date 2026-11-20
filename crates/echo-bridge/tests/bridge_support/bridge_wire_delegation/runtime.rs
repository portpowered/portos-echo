//! Component T3 functional proof through the production application host boundary.
//! Uses controlled Tokio deadlines and UTC, not a prebuilt-artifact integration claim.
use super::*;
use echo_bridge::{bridge_input, bridge_runtime};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::oneshot;

#[path = "runtime/emotion.rs"]
mod emotion;
#[path = "runtime/trace_checks.rs"]
mod trace_checks;

struct Application {
    directory: tempfile::TempDir,
    stop: oneshot::Sender<bool>,
    worker: tokio::task::JoinHandle<Result<(), echo_bridge::Error>>,
    satellite: bridge_short::Satellite,
    day: Arc<AtomicU64>,
    spoof: Spoof,
    model: TcpListener,
    live: bridge_peer::Live,
}

impl Application {
    async fn wait_event(&self, event: &str) {
        bridge_wire_auth_failure::trace_wait::observe(&self.path(), event, || {
            if self.worker.is_finished() {
                "application task finished (no child process)".into()
            } else {
                "application task running (no child process)".into()
            }
        })
        .await;
    }

    async fn start(cap: u64, deadline: u64) -> Self {
        let mut application = Self::idle(cap, deadline).await;
        application.wake().await;
        application
    }

    async fn idle(cap: u64, deadline: u64) -> Self {
        Self::idle_with_emotion(cap, deadline, false).await
    }

    async fn idle_with_emotion(cap: u64, deadline: u64, emotion: bool) -> Self {
        Self::idle_options(cap, deadline, emotion, true).await
    }

    async fn idle_options(cap: u64, deadline: u64, emotion: bool, payloads: bool) -> Self {
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
        settings.config = Some(echo_config::BridgeConfig::load(directory.path().join("bridge.toml")).unwrap());
        settings.config.as_mut().unwrap().trace.payloads = payloads;
        if emotion {
            settings.delegation.as_mut().unwrap().1.allow_tools = vec!["ui.show_emotion".into()];
        }
        let agent = &mut settings.delegation.as_mut().unwrap().0;
        agent.daily_request_cap = cap;
        agent.task_deadline_seconds = deadline;
        let day = Arc::new(AtomicU64::new(17));
        let source = day.clone();
        let (stop, shutdown) = oneshot::channel();
        let connector = echo_wyoming::adapter::connector(settings.wyoming, echo_wyoming::ClientOptions::default());
        let worker = tokio::spawn(bridge_runtime::execute_controlled(
            settings,
            bridge_runtime::Control {
                utc_day: Arc::new(move || source.load(Ordering::SeqCst)),
                shutdown,
            },
            connector,
        ));
        let mut satellite = listener.accept().await.unwrap().0;
        assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "run-satellite");
        assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "describe");
        bridge_short::send(&mut satellite, "info", json!({"satellite":{"name":"synthetic"}})).await;
        let satellite = bridge_short::Satellite::new(satellite);
        Self {
            directory,
            stop,
            worker,
            satellite,
            day,
            spoof,
            model,
            live,
        }
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
        bridge_lifecycle::control(&mut self.satellite, "hassmic-flush").await;
        bridge_short::send(&mut self.satellite, "played", json!({})).await;
        bridge_short::send(&mut self.satellite, "audio-stop", json!({})).await;
        loop {
            if trace(&self.path())
                .iter()
                .any(|e| e["event"] == "reply.played" && e["run_id"] == run)
            {
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        bridge_lifecycle::control(&mut self.satellite, "hassmic-end").await;
    }

    async fn finish(self) {
        self.finish_with_calls(0).await;
    }

    async fn finish_with_calls(self, calls: usize) {
        self.stop.send(true).unwrap();
        assert_eq!(self.worker.await.unwrap(), Ok(()));
        assert_eq!(self.spoof.snapshot().unwrap().calls.len(), calls);
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
        startup: None,
        runner_skills: Default::default(),
        config: None,
        settings_warning: None,
        sim_clock: false,
        sim_epoch: None,
        mcp: None,
        muse: None,
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
    let mut app = Application::start(3, 60).await;
    let mut stalled = app.request("deadline").await;
    elapse(Duration::from_secs(59)).await;
    assert!(app.live.commentary.try_recv().is_err());
    assert!(!trace(&app.path()).iter().any(|e| e["event"] == "run.failed"));
    elapse(Duration::from_secs(1)).await;
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
    trace_checks::resolved(&app.path(), "deadline", "timed_out", "submitted");
    app.finish().await;
}

#[tokio::test]
#[ignore = "T3 controlled application and loopback peers"]
async fn bridge_wire_runtime_abandon_replacement_retains_budget_and_discards_late_result() {
    let mut app = Application::start(1, 60).await;
    let mut old = app.request("old").await;
    elapse(Duration::from_secs(12)).await;
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
    app.wait_event("session.commentary.append").await;
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
    trace_checks::resolved(&app.path(), "old", "cancelled", "unobserved");
    trace_checks::resolved(&app.path(), "limited", "failed", "submitted");
    trace_checks::resolved(&app.path(), "fresh", "completed", "submitted");
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
    bridge_lifecycle::control(&mut app.satellite, "hassmic-flush").await;
    bridge_short::send(&mut app.satellite, "played", json!({})).await;
    bridge_short::send(&mut app.satellite, "audio-stop", json!({})).await;
    app.wait_event("run.failed").await;
    bridge_refusal::error(&mut app.satellite, false).await;
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
    let resolved = trace_checks::resolved(&app.path(), "oversize", "failed", "failed");
    assert_eq!(resolved["reason_class"], "append_failed");
    assert_eq!(resolved["answer"]["text"], "private provider text".repeat(900));
    assert!(app.live.commentary.try_recv().is_err());
    assert_no_request(&app.model).await;
    app.wake().await;
    let mut recovered = app.request("recovered").await;
    respond(&mut recovered, "fresh result").await.unwrap();
    app.commentary("recovered", "fresh result").await;
    assert_eq!(app.live.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    app.finish().await;
}

// Prevent Tokio auto-advance while the OS delivers loopback packets. The ready
// spinner is owned and aborted before returning to the real I/O clock.
async fn elapse(duration: Duration) {
    let active = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    tokio::time::pause();
    tokio::time::advance(duration).await;
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    tokio::time::resume();
    active.abort();
}

#[tokio::test]
#[ignore = "T3 controlled application and loopback peers"]
async fn bridge_wire_runtime_long_delegation_plays_on_same_call_and_requires_played() {
    let mut app = Application::start(3, 60).await;
    elapse(Duration::from_secs(8)).await;
    let mut model = app.request("slow").await;
    elapse(Duration::from_secs(12)).await;
    respond(&mut model, "slow result").await.unwrap();
    app.commentary("slow", "slow result").await;
    app.live.events.send("synthetic.more_audio".into()).await.unwrap();
    let path = app.path();
    bridge_short::receive_short(&mut app.satellite, &app.live, &path, 32).await;
    bridge_short::send(&mut app.satellite, "played", json!({})).await;
    bridge_short::send(&mut app.satellite, "audio-stop", json!({})).await;
    app.wait_event("reply.played").await;
    let entries = trace(&app.path());
    assert!(!entries.iter().any(|e| e["event"] == "run.failed"));
    let edges: Vec<_> = entries
        .iter()
        .filter(|e| {
            matches!(
                e["event"].as_str(),
                Some("session.commentary.append" | "reply.started" | "reply.played")
            )
        })
        .map(|e| (e["event"].clone(), e["run_id"].clone()))
        .collect();
    assert_eq!(
        edges,
        vec![
            (json!("session.commentary.append"), json!("run-1")),
            (json!("reply.started"), json!("run-1")),
            (json!("reply.played"), json!("run-1")),
        ]
    );
    assert_eq!(app.live.calls.load(Ordering::SeqCst), 1);
    assert_no_request(&app.model).await;
    app.finish().await;
}

#[tokio::test]
#[ignore = "T3 controlled application and loopback peers"]
async fn bridge_wire_runtime_long_delegation_missing_played_is_bounded_failure() {
    let mut app = Application::start(3, 60).await;
    let mut model = app.request("slow").await;
    elapse(Duration::from_secs(12)).await;
    respond(&mut model, "slow result").await.unwrap();
    app.commentary("slow", "slow result").await;
    app.live
        .events
        .send(json!({"type":"response.audio.done"}).to_string())
        .await
        .unwrap();
    assert_eq!(
        bridge_short::read_reply(&mut app.satellite).await.0["type"],
        "audio-stop"
    );
    elapse(Duration::from_secs(5)).await;
    app.wait_event("run.failed").await;
    let entries = trace(&app.path());
    assert_eq!(entries.iter().filter(|e| e["event"] == "reply.played").count(), 0);
    assert_eq!(
        entries.iter().find(|e| e["event"] == "run.failed").unwrap()["reason"],
        "deadline"
    );
    app.finish().await;
}

#[tokio::test]
#[ignore = "T3 controlled application and loopback peers"]
async fn bridge_wire_runtime_long_delegation_kill_abandons_and_shutdown_remains_responsive() {
    let mut app = Application::start(3, 60).await;
    let mut model = app.request("killed").await;
    elapse(Duration::from_secs(12)).await;
    std::fs::create_dir_all(app.directory.path().join("audit")).unwrap();
    std::fs::write(app.directory.path().join("audit/bridge-kill"), []).unwrap();
    bridge_lifecycle::control(&mut app.satellite, "hassmic-flush").await;
    assert_eq!(model.read(&mut [0]).await.unwrap(), 0);
    bridge_short::send(&mut app.satellite, "played", json!({})).await;
    bridge_short::send(&mut app.satellite, "audio-stop", json!({})).await;
    app.wait_event("live.closed").await;
    let entries = trace(&app.path());
    assert_eq!(
        entries.iter().find(|e| e["event"] == "live.closed").unwrap()["reason"],
        "kill"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|e| e["event"] == "session.commentary.append")
            .count(),
        0
    );
    trace_checks::resolved(&app.path(), "killed", "cancelled", "unobserved");
    assert!(app.live.commentary.try_recv().is_err());
    tokio::time::timeout(Duration::from_secs(1), app.finish())
        .await
        .unwrap();
}

// T3 controlled: the real connector and application observe unchanged probe deadlines.
#[tokio::test]
async fn bridge_satellite_probe_idle_stays_connected_then_suppression_reconnects() {
    let clock = bridge_short::ReadyClock::new();
    let mut app = Application::idle(8, 60).await;
    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let address = trace(&app.path())[0]["health_addr"].as_str().unwrap().to_owned();
    for _ in 0..170 {
        next_probe(&mut app.satellite, &address).await;
    }
    assert_health(&address).await;
    let deadline = echo_wyoming::ClientOptions::default().operation_timeout;
    assert!(started.elapsed() > 3 * deadline);
    let entries = trace(&app.path());
    assert_eq!(
        entries.iter().filter(|e| e["event"] == "wyoming.reconnecting").count(),
        0
    );
    assert_eq!(entries.iter().filter(|e| e["event"] == "run.started").count(), 0);
    app.satellite.responding(false).await;
    next_probe(&mut app.satellite, &address).await;
    assert_eq!(bridge_short::read(&mut app.satellite).await.0["type"], "describe");
    tokio::time::advance(deadline + Duration::from_secs(1)).await;
    loop {
        let response = health(&address, "GET", "/healthz").await;
        let body: Value = serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
        if body["wyoming"] == "reconnecting" {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        trace(&app.path())
            .iter()
            .filter(|e| e["event"] == "wyoming.reconnecting")
            .count(),
        1
    );
    app.finish().await;
    drop(clock);
}

async fn next_probe(satellite: &mut bridge_short::Satellite, address: &str) {
    let start = tokio::time::Instant::now();
    while !satellite.pending_probe() {
        // A real health response lets loopback readiness progress between small
        // steps; a serviced probe is the barrier, not a number of yields.
        health(address, "GET", "/healthz").await;
        tokio::time::advance(Duration::from_millis(100)).await;
        assert!(start.elapsed() < Duration::from_secs(5), "probe service stalled");
    }
    satellite.probe().await;
}
