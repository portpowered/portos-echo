//! T3 fault injection only at the authenticated external Muse HTTP boundary.
//! Uses the production bridge binary; does not claim daemon behavior for faults.
use super::*;

struct Peer {
    child: Process,
    trust: Muse,
    url: String,
}

impl Peer {
    async fn start(mode: &str) -> Self {
        let trust = Muse::start("subjectAltName=IP:127.0.0.1").await;
        let mut command = Command::new("python3");
        command
            .current_dir(trust.directory.path())
            .arg(root().join("crates/echo-bridge/tests/bridge_support/bridge_peer/bridge_muse_tests/tls_peer.py"))
            .arg(mode);
        let mut child = Process::spawn(&mut command);
        let port: u16 = child.line().await.parse().unwrap();
        let origin = "https://127.0.0.1";
        Self {
            child,
            trust,
            url: format!("{origin}:{port}"),
        }
    }

    async fn close(mut self, gets: usize) {
        let requests: Value = serde_json::from_str(&self.child.command("stop").await).unwrap();
        let requests = requests.as_array().unwrap();
        assert_eq!(requests.len(), 1 + gets);
        assert_eq!(requests[0]["method"], "POST");
        assert_eq!(requests[0]["path"], "/v1/tasks");
        for request in &requests[1..] {
            assert_eq!(request, &json!({"method":"GET","path":"/v1/tasks/task-pending/events"}));
        }
        self.child.finish(0).await;
        assert_eq!(self.trust.calls(), Vec::<Value>::new());
        self.trust.stop().await;
    }
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host and prebuilt bridge/Muse binaries; Linux, openssl, python3"]
async fn muse_invalid_events_and_interrupted_admission_fail_without_retry() {
    for mode in [
        "wrong_id",
        "invalid",
        "oversized",
        "unavailable",
        "interrupted",
        "duplicate",
        "stalled",
    ] {
        journey(mode).await;
    }
}

async fn journey(mode: &str) {
    let mut peer = Peer::start(mode).await;
    let directory = tempfile::tempdir().unwrap();
    let mut host = Process::spawn(&mut host());
    setup(directory.path(), &mut host, &peer).await;
    let model = bridge_wire_delegation::Model::start(false, true).await;
    let mut live = bridge_peer::Live::bind_frames(0).await;
    let mut bridge = start_bridge(directory.path(), &live.url, &model.origin, mode);
    let path = directory.path().join("trace");
    wake(&mut bridge, &mut host, &live, &path).await;
    advance_stalled(mode, &mut peer.child, &mut bridge).await;
    finish(mode, &mut live, &mut bridge, &mut host, &path, &model).await;
    bridge.finish(0).await;
    host.finish(0).await;
    peer.close(usize::from(mode != "interrupted")).await;
}

async fn wake(bridge: &mut Process, host: &mut Process, live: &bridge_peer::Live, path: &Path) {
    assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
    assert_eq!(host.command("wake").await, "wake");
    wait(path, "mic.frames").await;
    live.events
        .send(bridge_wire_delegation::event("muse-d-1", "client"))
        .await
        .unwrap();
}

fn start_bridge(directory: &Path, live: &str, model: &str, mode: &str) -> Process {
    let mut command = bridge_wire_delegation::run_command(directory, live, model);
    if mode == "stalled" {
        command.arg("--sim-clock");
    }
    Process::spawn(&mut command)
}

async fn advance_stalled(mode: &str, peer: &mut Process, bridge: &mut Process) {
    if mode != "stalled" {
        return;
    }
    assert_eq!(peer.command("waiting").await, "waiting");
    let response = bridge
        .command(r#"{"action":"advance_clock","milliseconds":5000}"#)
        .await;
    assert_eq!(
        serde_json::from_str::<Value>(&response).unwrap(),
        json!({"event":"bridge.clock_advanced","milliseconds":5000})
    );
}

async fn setup(directory: &Path, host: &mut Process, peer: &Peer) {
    let port = host.line().await.parse().unwrap();
    configure(directory, port, &peer.trust, "");
    let config = directory.join("bridge.toml");
    let text = std::fs::read_to_string(&config)
        .unwrap()
        .replace(&peer.trust.url, &peer.url);
    std::fs::write(config, text).unwrap();
}

async fn finish(
    mode: &str,
    live: &mut bridge_peer::Live,
    bridge: &mut Process,
    host: &mut Process,
    path: &Path,
    model: &bridge_wire_delegation::Model,
) {
    let text = if mode == "duplicate" {
        "Muse test job completed"
    } else {
        "Muse is unavailable. Please try again."
    };
    reply(live, bridge, host, path, text).await;
    let outcome = if mode == "duplicate" { "completed" } else { "failed" };
    let reason = if mode == "interrupted" {
        "submission_failed"
    } else {
        "task_failed"
    };
    check_resolution(
        path,
        outcome,
        "submitted",
        Some(text),
        (outcome == "failed").then_some(reason),
    );
    let entries = trace(path);
    let terminals: Vec<_> = entries.iter().filter(|e| e["event"] == "muse.terminal").collect();
    let admitted = mode != "interrupted";
    assert_eq!(
        entries.iter().filter(|e| e["event"] == "muse.submit").count(),
        usize::from(admitted)
    );
    assert_eq!(terminals.len(), usize::from(admitted));
    if let Some(terminal) = terminals.first() {
        terminal_status(mode, terminal);
    }
    assert_eq!(
        entries
            .iter()
            .filter(|e| e["event"] == "session.commentary.append")
            .count(),
        1
    );
    assert_eq!(entries.iter().filter(|e| e["event"] == "tools/call").count(), 0);
    assert_eq!(model.request_count(), 0);
    let raw = std::fs::read_to_string(path).unwrap();
    for secret in [
        "Private guessed success",
        "Private provider response",
        "synthetic-api-key",
    ] {
        assert!(!raw.contains(secret));
    }
    evidence(&format!(
        "muse {mode}: task_id={} submits=1 polls={} terminal={} completed={} commentary=1 pcm_bytes=9600 model_requests=0",
        if admitted { "task-pending" } else { "unadmitted" },
        usize::from(admitted),
        usize::from(admitted),
        usize::from(mode == "duplicate")
    ));
}

fn terminal_status(mode: &str, terminal: &Value) {
    assert_eq!(terminal["task_id"], "task-pending");
    assert_eq!(terminal["delegation_id"], "muse-d-1");
    assert_eq!(
        terminal["status"],
        if mode == "duplicate" { "completed" } else { "failed" }
    );
    if mode != "duplicate" {
        assert_eq!(terminal["retryable"], false);
    }
}
