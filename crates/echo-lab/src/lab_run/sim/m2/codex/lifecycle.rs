//! T3: prebuilt bridge, real pin/media, controlled Wyoming and provider boundaries.
use super::*;

#[test]
fn playout_receipts_keep_earcon_separate_and_reject_unknown_or_missing_sources() {
    let mut clock = json!({"pcm":[],"states":[]});
    let row = json!({"pcm":[{"source":"earcon","pcm_bytes":960},
        {"source":"reply","pcm_bytes":960}],"states":[{"state":{"state":"listening"}}]});
    collect_observations(&mut clock, &row).unwrap();
    assert_eq!(clock["pcm"], json!([{"source":"reply","pcm_bytes":960}]));
    assert_eq!(clock["earcon_pcm"], json!([{"source":"earcon","pcm_bytes":960}]));
    assert_eq!(clock["states"], row["states"]);
    for source in [Value::Null, json!("unknown")] {
        let mut wrong = row.clone();
        wrong["pcm"][0]["source"] = source;
        assert_eq!(
            collect_observations(&mut clock, &wrong),
            Err("playout: unknown PCM source".into())
        );
    }
}
use tokio::{net::TcpListener, sync::mpsc};
mod overlap;

pub(super) struct Owner {
    root: config::Root,
    output: tempfile::TempDir,
    bridge: Process,
    collected: std::sync::mpsc::Receiver<std::process::ExitStatus>,
    provider: provider::Provider,
    wake: mpsc::Sender<()>,
    satellite: tokio::task::JoinHandle<usize>,
    address: std::net::SocketAddr,
    listener_lifetime: std::sync::Weak<TcpListener>,
    screen: Screen,
}

impl Owner {
    pub(super) async fn start(idle: u16, earcon: bool) -> Self {
        let options = super::super::tests::options();
        let root = config::Root::new().unwrap();
        let output = tempfile::tempdir().unwrap();
        let (listener, listener_lifetime) = observed_listener().await;
        let address = listener.local_addr().unwrap();
        let provider = provider::Provider::start_retained(&options.python).await.unwrap();
        let mut command = tokio::process::Command::new(options.bridge.unwrap());
        command = super::tests::continuous_command(
            command.as_std().get_program().as_ref(),
            &root.0,
            output.path(),
            address,
            &provider.origin,
        );
        let path = root.0.join("configured.private/bridge.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            path,
            text.replace("session_idle_seconds = 0", &format!("session_idle_seconds = {idle}"))
                .replace("wake_earcon = false", &format!("wake_earcon = {earcon}")),
        )
        .unwrap();
        let (wake, requested) = mpsc::channel(1);
        let satellite = tokio::spawn(satellite(listener, requested, earcon));
        let mut bridge = Process::start(&mut command).unwrap();
        let collected = bridge.observe_collection();
        bridge.expect("bridge.ready").await.unwrap();
        let screen = paired_screen(&output.path().join("bridge.private.jsonl")).await;
        Self {
            root,
            output,
            bridge,
            collected,
            provider,
            wake,
            satellite,
            address,
            listener_lifetime,
            screen,
        }
    }

    pub(super) async fn run(&mut self, index: usize) {
        self.wake.send(()).await.unwrap();
        if index > 1 {
            self.provider.next_capture().await;
        }
        let completed = self.bridge.expect("bridge.run_completed").await;
        if let Err(error) = &completed {
            let exited = self.bridge.exited();
            assert!(
                completed.is_ok(),
                "M2 run {index}: {error}; exited={exited:?}; status={}; trace={:?}",
                self.bridge.exit_summary(),
                self.trace().records
            );
        }
        let row = completed.unwrap();
        assert_eq!(row["run_id"], format!("run-{index}"));
        let states: &[&str] = if index == 1 {
            &["listening", "connected", "ready"]
        } else {
            &["listening", "ready"]
        };
        for state in states {
            assert_eq!(self.screen.next().await.unwrap()["state"]["state"], *state);
        }
    }
    /// The bridge is still running whenever an owner reads its trace.
    pub(super) fn trace(&self) -> Trace {
        running_trace(&self.output.path().join("bridge.private.jsonl")).unwrap()
    }

    pub(super) async fn close(mut self, expected_calls: usize) {
        let trace = self.trace();
        let origin = self.provider.origin.clone();
        self.provider.quiesce().await.unwrap();
        assert_eq!(self.bridge.close().await, Ok(()));
        drop(self.bridge);
        assert!(self.collected.try_recv().unwrap().success());
        assert!(self.listener_lifetime.upgrade().is_some());
        drop(self.wake);
        assert_eq!(self.satellite.await.unwrap(), expected_calls);
        let observed = self.provider.close().await.unwrap();
        assert_eq!(
            observed,
            json!({"event":"provider.snapshot", "calls":1, "matching_calls":1, "sidebands":1, "external":0, "refreshes":0})
        );
        assert!(self.listener_lifetime.upgrade().is_none());
        remove_root(self.root);
        overlap::report(&trace, &origin, &observed);
    }
}

async fn satellite(listener: Arc<TcpListener>, mut requested: mpsc::Receiver<()>, earcon: bool) -> usize {
    use super::tests::{read, send};
    let mut stream = listener.accept().await.unwrap().0;
    assert_eq!(read(&mut stream).await.0["type"], "run-satellite");
    assert_eq!(read(&mut stream).await.0["type"], "describe");
    send(&mut stream, "info", json!({"satellite":{"name":"synthetic"}}), vec![]).await;
    let wake: Value = serde_json::from_str(
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/wyoming/wake-word.jsonl"
        ))
        .lines()
        .nth(1)
        .unwrap(),
    )
    .unwrap();
    let mut completed = 0;
    while wait_for_wake(&mut stream, requested.recv()).await {
        send(&mut stream, "run-pipeline", wake["header"]["data"].clone(), vec![]).await;
        stream = run_playback(stream, earcon).await;
        completed += 1;
    }
    // Socket EOF does not release this test-owned listener. The controller
    // explicitly drops its wake sender after collecting the bridge.
    assert!(requested.recv().await.is_none());
    drop(listener);
    completed
}

async fn run_playback(mut stream: tokio::net::TcpStream, earcon: bool) -> tokio::net::TcpStream {
    let format = json!({"rate":16000,"width":2,"channels":1});
    super::tests::send(&mut stream, "audio-start", format.clone(), vec![]).await;
    if earcon {
        super::wake::acknowledge(&mut stream).await;
    }
    let (stream, frames) = super::tests::continuous_playback(stream, format).await;
    assert_eq!(frames, 0);
    stream
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn codex_lifecycle_idle_probe_completes_before_first_wake() {
    tokio::time::timeout(DEADLINE, async {
        let mut owner = Owner::start(120, false).await;
        loop {
            let trace = owner.trace();
            assert!(
                !owner.bridge.exited().unwrap(),
                "idle bridge exited: {:?}",
                trace.records
            );
            assert_eq!(trace.records.iter().filter(|r| r["event"] == "run.started").count(), 0);
            if trace.records.iter().any(|r| r["event"] == "wyoming.probe_ok") {
                break;
            }
            tokio::task::yield_now().await;
        }
        owner.run(1).await;
        owner.close(1).await;
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn codex_lifecycle_reuses_call_then_stops_once_at_idle() {
    tokio::time::timeout(
        super::DEADLINE,
        Box::pin(async {
            let mut owner = Owner::start(1, false).await;
            owner.run(1).await;
            owner.run(2).await;
            owner.provider.quiesce().await.unwrap();
            owner.bridge.expect("bridge.ready").await.unwrap();
            let rows = owner.trace();
            let stops: Vec<_> = rows.records.iter().filter(|r| r["event"] == "live.closed").collect();
            assert_eq!(stops.len(), 1);
            assert_eq!(stops[0]["reason"], "idle");
            assert_eq!(rows.records.iter().filter(|r| r["event"] == "reply.played").count(), 2);
            owner.close(2).await;
        }),
    )
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn codex_lifecycle_kill_aborts_reused_active_run_without_success_or_late_audio() {
    tokio::time::timeout(
        super::DEADLINE,
        Box::pin(async {
            let mut owner = Owner::start(120, false).await;
            owner.run(1).await;
            owner.provider.quiesce().await.unwrap();
            owner.wake.send(()).await.unwrap();
            assert_eq!(owner.screen.next().await.unwrap()["state"]["state"], "listening");
            let options = super::super::tests::options();
            let output = tokio::process::Command::new(options.bridge.unwrap())
                .current_dir(owner.root.0.join("configured.private"))
                .args(["kill", "--config", "bridge.toml"])
                .output()
                .await
                .unwrap();
            assert_eq!(output.status.code(), Some(0));
            assert_eq!(output.stdout, b"kill-switch: on\n");
            assert_eq!(output.stderr, b"");
            assert_eq!(
                owner.screen.next().await.unwrap()["state"],
                json!({
                    "state":"error", "relink":false,
                    "message":"Kill switch enabled. Run echo-bridge kill --clear with your config."
                })
            );
            let rows = owner.trace();
            let stops: Vec<_> = rows.records.iter().filter(|r| r["event"] == "live.closed").collect();
            assert_eq!(stops.len(), 1);
            assert_eq!(stops[0]["reason"], "kill");
            assert_eq!(rows.records.iter().filter(|r| r["event"] == "reply.played").count(), 1);
            assert_eq!(
                rows.records
                    .iter()
                    .filter(|r| r["event"] == "bridge.run_completed")
                    .count(),
                1
            );
            owner.close(2).await;
        }),
    )
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn codex_lifecycle_two_owners_survivor_reuses_own_call_after_other_closes() {
    tokio::time::timeout(
        super::DEADLINE,
        Box::pin(async {
            let (mut first, mut second) = tokio::join!(Owner::start(120, false), Owner::start(120, false));
            assert_ne!(first.address, second.address);
            assert_ne!(first.provider.origin, second.provider.origin);
            assert_ne!(first.root.0, second.root.0);
            tokio::join!(first.run(1), second.run(1));
            first.close(1).await;
            second.run(2).await;
            let rows = second.trace();
            assert_eq!(rows.records.iter().filter(|r| r["event"] == "live.closed").count(), 0);
            assert_eq!(rows.records.iter().filter(|r| r["event"] == "reply.played").count(), 2);
            assert_eq!(rows.records.iter().filter(|r| r["event"] == "tools/call").count(), 0);
            second.close(2).await;
        }),
    )
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn codex_lifecycle_call_refusal_and_malformed_sdp_never_connect_or_play() {
    tokio::time::timeout(
        super::DEADLINE,
        Box::pin(async {
            for fault in ["call_refused", "malformed_sdp"] {
                failed_call(fault, false).await;
            }
        }),
    )
    .await
    .unwrap();
}

pub(super) async fn failed_call(fault: &str, earcon: bool) {
    let options = super::super::tests::options();
    let root = config::Root::new().unwrap();
    let output = tempfile::tempdir().unwrap();
    let (listener, listener_lifetime) = observed_listener().await;
    let address = listener.local_addr().unwrap();
    let provider = provider::Provider::start_fault(&options.python, fault).await.unwrap();
    let mut command = super::tests::continuous_command(
        options.bridge.as_ref().unwrap(),
        &root.0,
        output.path(),
        address,
        &provider.origin,
    );
    let config = root.0.join("configured.private/bridge.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        config,
        text.replace("wake_earcon = false", &format!("wake_earcon = {earcon}")),
    )
    .unwrap();
    let (wake, requested) = tokio::sync::oneshot::channel();
    let satellite = tokio::spawn(failure_satellite(listener, requested, earcon));
    let mut bridge = Process::start(&mut command).unwrap();
    let collected = bridge.observe_collection();
    bridge.expect("bridge.ready").await.unwrap();
    let path = output.path().join("bridge.private.jsonl");
    let mut screen = paired_screen(&path).await;
    assert!(listener_lifetime.upgrade().is_some());
    wake.send(()).unwrap();
    for expected in ["listening", "error"] {
        let frame = screen
            .next()
            .await
            .map_err(|error| {
                format!(
                    "fault={fault} earcon={earcon} expected={expected}: {error}; trace={:?}",
                    read_trace(&path).map(|trace| trace.records)
                )
            })
            .unwrap();
        assert_eq!(frame["state"]["state"], expected, "fault={fault} earcon={earcon}");
    }
    failed_process(bridge, collected).await;
    satellite.await.unwrap();
    let observed = provider.close().await.unwrap();
    failure_receipts(&observed, &path);
    reopened_status(options.bridge.as_ref().unwrap(), &root.0).await;
    assert!(listener_lifetime.upgrade().is_none());
    remove_root(root);
}

async fn failure_satellite(listener: Arc<TcpListener>, wake: tokio::sync::oneshot::Receiver<()>, earcon: bool) {
    use super::tests::{read, send};
    let mut stream = listener.accept().await.unwrap().0;
    assert_eq!(read(&mut stream).await.0["type"], "run-satellite");
    assert_eq!(read(&mut stream).await.0["type"], "describe");
    send(&mut stream, "info", json!({"satellite":{"name":"synthetic"}}), vec![]).await;
    assert!(wait_for_wake(&mut stream, async { wake.await.ok() }).await);
    send(
        &mut stream,
        "run-pipeline",
        json!({"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"alexa"}),
        vec![],
    )
    .await;
    while stream.peek(&mut [0]).await.unwrap() != 0 {
        let (row, bytes) = read(&mut stream).await;
        assert_eq!(
            bytes.len(),
            if row["type"] == "audio-chunk" && earcon { 960 } else { 0 }
        );
        // Withhold Played: neither a failed call nor an unacknowledged
        // acknowledgement may manufacture successful playback.
    }
}

fn failure_receipts(observed: &Value, path: &Path) {
    let trace = read_trace(path).unwrap();
    assert!(!std::fs::read_to_string(path).unwrap().contains("synthetic private"));
    assert_eq!(
        observed["calls"],
        json!([{
            "path":"/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas",
            "model":"gpt-live-1-codex", "alpha":"quicksilver=v2", "nominated_key":true,
            "account_header":null
        }])
    );
    assert_eq!(observed["external"], 0);
    assert_eq!(observed["refreshes"], 0);
    for event in [
        "reply.started",
        "reply.played",
        "bridge.run_completed",
        "tools/call",
        "wake.earcon_played",
        "call.live_connected",
    ] {
        assert_eq!(trace.records.iter().filter(|r| r["event"] == event).count(), 0);
    }
}

pub(super) async fn paired_screen(path: &Path) -> Screen {
    let trace = read_trace(path).unwrap();
    let address = trace.records.iter().find(|r| r["event"] == "screen.listening").unwrap()["address"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let screen = Screen::connect(address, true).await.unwrap();
    assert_eq!(screen.current().unwrap()["state"]["state"], "ready");
    screen
}

pub(super) async fn reopened_status(binary: &Path, root: &Path) {
    let status = tokio::process::Command::new(binary)
        .current_dir(root.join("configured.private"))
        .args(["status", "--config", "bridge.toml"])
        .output()
        .await
        .unwrap();
    assert_eq!(status.status.code(), Some(0));
    assert_eq!(status.stdout, b"mode: api-key\naccount: n/a\nexpires: n/a\n");
    assert_eq!(status.stderr, b"");
}

async fn failed_process(mut bridge: Process, collected: std::sync::mpsc::Receiver<std::process::ExitStatus>) {
    assert_eq!(bridge.close().await, Err("cleanup: owned child exited nonzero".into()));
    drop(bridge);
    assert_eq!(collected.try_recv().unwrap().code(), Some(1));
}

pub(super) async fn observed_listener() -> (Arc<TcpListener>, std::sync::Weak<TcpListener>) {
    let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await.unwrap());
    let lifetime = Arc::downgrade(&listener);
    (listener, lifetime)
}

// A synthetic satellite must keep answering the real bridge's idle probes
// while the test controls wake admission. Peek is cancellation safe: a wake
// cannot discard a partially read frame that playback still needs to consume.
pub(super) async fn wait_for_wake(
    stream: &mut tokio::net::TcpStream,
    requested: impl std::future::Future<Output = Option<()>>,
) -> bool {
    use super::tests::{read, send};
    tokio::pin!(requested);
    loop {
        let mut byte = [0];
        tokio::select! {
            requested = &mut requested => return requested.is_some(),
            ready = stream.peek(&mut byte) => if ready.unwrap() == 0 { return false; },
        }
        let (row, payload) = read(stream).await;
        assert_eq!(payload, [] as [u8; 0]);
        match row["type"].as_str().unwrap() {
            "describe" => send(stream, "info", json!({"satellite":{"name":"synthetic"}}), vec![]).await,
            "error" => assert_eq!(row["data"]["code"], "echo-kill-switch"),
            other => assert_eq!(other, "expected idle probe or kill refusal"),
        }
    }
}

#[tokio::test]
async fn t3_lifecycle_satellite_answers_probes_before_the_test_releases_wake() {
    use super::tests::{read, send};
    let (listener, lifetime) = observed_listener().await;
    let address = listener.local_addr().unwrap();
    let (wake, requested) = mpsc::channel(1);
    let task = tokio::spawn(satellite(listener, requested, false));
    let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
    send(&mut client, "run-satellite", json!({}), vec![]).await;
    for _ in 0..3 {
        send(&mut client, "describe", json!({}), vec![]).await;
        let row = tokio::time::timeout(DEADLINE, read(&mut client)).await.unwrap().0;
        assert_eq!(row["type"], "info");
        assert_eq!(row["data"]["satellite"]["name"], "synthetic");
    }
    drop(wake);
    assert_eq!(task.await.unwrap(), 0);
    assert!(lifetime.upgrade().is_none());
}

fn remove_root(root: config::Root) {
    let directory = root.0.clone();
    drop(root);
    assert!(!directory.exists());
}
