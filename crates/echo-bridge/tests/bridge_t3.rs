#![cfg(test)]
//! T3: prebuilt binary, pinned hassmic-host and synthetic Live on IPv4 loopback.
use echo_live::fake::{FakeLive, FakeResponse, FakeScript};
use serde_json::{Value, json};
use std::io::Write;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
};
#[path = "bridge_support/bridge_cut.rs"]
mod bridge_cut;
#[path = "bridge_support/bridge_latency.rs"]
mod bridge_latency;
#[path = "bridge_support/bridge_lifecycle.rs"]
mod bridge_lifecycle;
#[path = "bridge_support/bridge_peer.rs"]
mod bridge_peer;
#[path = "bridge_support/bridge_short.rs"]
mod bridge_short;
#[path = "bridge_support/bridge_spine_edges.rs"]
mod bridge_spine_edges;
#[path = "bridge_support/bridge_wire_auth.rs"]
mod bridge_wire_auth;
#[path = "bridge_support/bridge_wire_auth_failure.rs"]
mod bridge_wire_auth_failure;
#[path = "bridge_support/bridge_wire_delegation.rs"]
mod bridge_wire_delegation;
#[path = "bridge_support/bridge_wire_screen.rs"]
mod bridge_wire_screen;
// Observation ceiling for slow instrumented loopback runs; product deadlines stay unchanged.
const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(30);
/// Gap between re-reads of a trace file or health endpoint while waiting for the bridge.  A bare `yield_now` loop
/// re-read the whole file thousands of times a second and starved the coverage-instrumented bridge it was waiting on.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

#[path = "bridge_refusal/mod.rs"]
mod bridge_refusal;

struct Process {
    name: &'static str,
    expected: String,
    child: Child,
    input: Option<ChildStdin>,
    lines: Lines<BufReader<ChildStdout>>,
}
impl Process {
    fn spawn(command: &mut Command) -> Self {
        let name = if Path::new(command.as_std().get_program())
            .file_stem()
            .is_some_and(|name| name == "echo-bridge")
        {
            "echo-bridge"
        } else {
            "host controller"
        };
        let expected = if name == "echo-bridge" {
            "bridge.ready"
        } else {
            "host port"
        }
        .into();
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(name == "echo-bridge")
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let lines = BufReader::new(child.stdout.take().unwrap()).lines();
        Self {
            name,
            expected,
            child,
            input,
            lines,
        }
    }
    async fn line(&mut self) -> String {
        let response = tokio::time::timeout(OBSERVATION_TIMEOUT, self.lines.next_line()).await;
        assert!(
            response.is_ok(),
            "{} pid {:?}: timed out after 30s waiting for {}",
            self.name,
            self.child.id(),
            self.expected
        );
        let line = response.unwrap().unwrap();
        if self.name == "echo-bridge" {
            self.expected = "bridge.run_completed".into();
        }
        if line.is_none() {
            let mut error = String::new();
            self.child
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut error)
                .await
                .unwrap();
            assert_eq!(error, "", "controller or bridge exited before response");
        }
        line.unwrap()
    }
    async fn command(&mut self, command: &str) -> String {
        self.expected = format!("host command reply: {command}");
        self.input
            .as_mut()
            .unwrap()
            .write_all(format!("{command}\n").as_bytes())
            .await
            .unwrap();
        self.input.as_mut().unwrap().flush().await.unwrap();
        self.line().await
    }
    async fn finish(&mut self, code: i32) {
        drop(self.input.take());
        let status = tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        let mut error = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut error)
            .await
            .unwrap();
        assert_eq!(status.code(), Some(code), "{error}");
    }
}
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn host() -> Command {
    bridge_refusal::host_script("tests/bridge_host.sh")
}
fn bridge(port: &str, url: &str, trace: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_echo-bridge"));
    command
        .env("ECHO_LAB_WYOMING_ADDR", format!("127.0.0.1:{port}"))
        .env("ECHO_LAB_LIVE_URL", url)
        .env("ECHO_LAB_TRACE_PATH", trace);
    command
}
fn trace(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
async fn health(address: &str, method: &str, path: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream
        .write_all(format!("{method} {path} HTTP/1.1\r\nHost: local\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
}
async fn assert_health(address: &str) {
    let response = health(address, "GET", "/healthz").await;
    assert_eq!(
        serde_json::from_str::<Value>(response.split_once("\r\n\r\n").unwrap().1).unwrap(),
        json!({"status":"running","wyoming":"connected","live":"idle","relink_required":false,"kill_switch":false})
    );
    assert!(
        health(address, "POST", "/healthz")
            .await
            .starts_with("HTTP/1.1 405 Method Not Allowed\r\nAllow: GET\r\n")
    );
    assert!(
        health(address, "GET", "/missing")
            .await
            .ends_with("Content-Length: 0\r\nConnection: close\r\n\r\n")
    );
}
#[tokio::test]
#[ignore = "T3 requires make hassmic-host and prebuilt echo-bridge"]
async fn bridge_spine_host_echo_health_and_eof() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bridge.jsonl");
    let fake = FakeLive::bind(FakeScript {
        events: vec![],
        response: FakeResponse::Answer,
    })
    .await
    .unwrap();
    let observer = fake.observer();
    let mut host = Process::spawn(&mut host());
    let port = host.line().await;
    let mut bridge = Process::spawn(&mut bridge(&port, &fake.base_url(), &path));
    assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
    let entries = trace(&path);
    let address = entries[0]["health_addr"].as_str().unwrap();
    assert_health(address).await;
    assert_eq!(host.command("wake").await, "wake");
    assert_eq!(
        serde_json::from_str::<Value>(&bridge.line().await).unwrap(),
        json!({"event":"bridge.run_completed","run_id":"run-1"})
    );
    let entries = trace(&path);
    let calls = observer.calls().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].frames.len(), 32);
    assert_host_microphone(&entries, calls[0].frames.len());
    assert_eq!(host.command("pcm").await, "30720 0");
    bridge.finish(0).await;
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
    host.finish(0).await;
    fake.shutdown().await;
}
fn assert_host_microphone(entries: &[Value], receipt: usize) {
    let mic = entries.iter().find(|e| e["event"] == "mic.completed").unwrap();
    assert_eq!(mic["count"], 32);
    assert_eq!(mic["input_samples"], 10240);
    assert_eq!(mic["tail_samples"], 0);
    assert_eq!(mic["dropped_chunks"], 0);
    let snapshots: Vec<_> = entries.iter().filter(|e| e["event"] == "mic.frames").collect();
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0]["phase"], "first");
    assert_eq!(snapshots[0]["count"], 2);
    assert_eq!(snapshots[1]["phase"], "completed");
    assert_eq!(snapshots[1]["run_id"], "run-1");
    assert_eq!(snapshots[1]["count"].as_u64(), Some(receipt as u64));
    assert_eq!(snapshots[1]["count"], mic["count"]);
    writeln!(std::io::stdout(),
        "run-1 mic.frames.completed={} fake.received={} mic.completed={} dropped_chunks={} input_samples={} tail_samples={} PCM=30720 0",
        snapshots[1]["count"], receipt, mic["count"], mic["dropped_chunks"], mic["input_samples"], mic["tail_samples"]
    ).unwrap();
    let ordered: Vec<_> = entries
        .iter()
        .filter(|e| e["run_id"] == "run-1")
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        ordered,
        [
            "wyoming.run_started",
            "run.started",
            "mic.frames",
            "reply.started",
            "reply.played",
            "wyoming.mic_frames",
            "mic.completed",
            "mic.frames",
            "wyoming.run_ended"
        ]
    );
    let wire = entries.iter().find(|e| e["event"] == "wyoming.mic_frames").unwrap();
    assert_eq!(wire["count"], mic["received_chunks"]);
}
#[tokio::test]
#[ignore = "T3 requires make hassmic-host and prebuilt echo-bridge"]
async fn bridge_spine_authorization_is_safe_and_never_played() {
    failed_call(
        FakeResponse::Http {
            status: 401,
            body: "synthetic private body".into(),
            location: None,
        },
        vec![],
        "authorization",
    )
    .await;
}
async fn failed_call(response: FakeResponse, events: Vec<String>, reason: &str) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let fake = FakeLive::bind(FakeScript { events, response }).await.unwrap();
    let (_satellite, mut bridge) = bridge_short::satellite(&fake.base_url(), &path).await;
    // Keep stdin open: operational failure must not wait for the control reader.
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(12), bridge.child.wait())
            .await
            .unwrap()
            .unwrap()
            .code(),
        Some(1)
    );
    let entries = trace(&path);
    assert_eq!(entries.last().unwrap()["reason"], reason);
    assert_eq!(entries.iter().filter(|e| e["event"] == "reply.played").count(), 0);
    assert!(
        !std::fs::read_to_string(&path)
            .unwrap()
            .contains("synthetic private body")
    );
    fake.shutdown().await;
}
#[tokio::test]
#[ignore = "T3 requires make hassmic-host and prebuilt echo-bridge"]
async fn bridge_spine_provider_outage_is_safe_and_never_played() {
    failed_call(
        FakeResponse::Http {
            status: 503,
            body: "synthetic private body".into(),
            location: None,
        },
        vec![],
        "unavailable",
    )
    .await;
}
#[tokio::test]
#[ignore = "T3 requires make hassmic-host and prebuilt echo-bridge"]
async fn bridge_spine_provider_stall_has_a_deadline() {
    failed_call(FakeResponse::StallHttp, vec![], "deadline").await;
}
#[tokio::test]
#[ignore = "T3 requires make hassmic-host and prebuilt echo-bridge"]
async fn bridge_spine_malformed_event_is_safe_and_never_played() {
    failed_call(FakeResponse::Answer, vec!["synthetic private body".into()], "decode").await;
}
#[tokio::test]
#[ignore = "T3 requires make hassmic-host and prebuilt echo-bridge"]
async fn bridge_spine_empty_explicit_completion_waits_for_host_played() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bridge.jsonl");
    let fake = FakeLive::bind(FakeScript {
        events: vec![json!({"type":"response.audio.done"}).to_string()],
        response: FakeResponse::Answer,
    })
    .await
    .unwrap();
    let mut host = Process::spawn(&mut host());
    let port = host.line().await;
    let mut bridge = Process::spawn(&mut bridge(&port, &fake.base_url(), &path));
    assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
    assert_eq!(host.command("wake").await, "wake");
    assert_eq!(
        serde_json::from_str::<Value>(&bridge.line().await).unwrap(),
        json!({"event":"bridge.run_completed","run_id":"run-1"})
    );
    let entries = trace(&path);
    assert_eq!(entries.iter().filter(|e| e["event"] == "reply.played").count(), 1);
    assert_eq!(host.command("pcm").await, "0 0");
    bridge.finish(0).await;
    host.finish(0).await;
    fake.shutdown().await;
}
#[tokio::test]
#[ignore = "T3 requires make hassmic-host and prebuilt echo-bridge"]
async fn bridge_wire_journey_pinned_host_delegation_screen_and_played() {
    let directory = tempfile::tempdir().unwrap();
    let mut host = Process::spawn(&mut host());
    let port = host.line().await.parse().unwrap();
    let spoof = echo_spoof_mcp::Spoof::start(echo_spoof_mcp::Options {
        expected_bearer: Some(echo_contracts::secret::Secret::new("synthetic-mcp-token".into())),
        ..echo_spoof_mcp::Options::default()
    })
    .await
    .unwrap();
    bridge_wire_delegation::configure(directory.path(), port, "api_key", &spoof, true).await;
    let model = bridge_wire_delegation::Model::start(false, true).await;
    let mut live = bridge_peer::Live::bind_frames(0).await;
    let mut bridge = Process::spawn(&mut bridge_wire_delegation::run_command(
        directory.path(),
        &live.url,
        &model.origin,
    ));
    assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
    let path = directory.path().join("trace");
    let mut screen = wire_journey_screen(directory.path()).await;
    assert_eq!(host.command("wake").await, "wake");
    bridge_wire_screen::listening_then_connected(&mut screen).await;
    bridge_wire_auth_failure::wait_event(&path, "mic.frames").await;
    live.events
        .send(bridge_wire_delegation::event("d-1", "client"))
        .await
        .unwrap();
    wire_journey_commentary(&mut live).await;
    live.events.send("synthetic.more_audio".into()).await.unwrap();
    bridge_wire_auth_failure::wait_event(&path, "reply.started").await;
    assert_eq!(
        bridge_wire_screen::state(&mut screen).await,
        json!({"state":"speaking"})
    );
    live.events
        .send(json!({"type":"response.audio.done"}).to_string())
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&bridge.line().await).unwrap(),
        json!({"event":"bridge.run_completed","run_id":"run-1"})
    );
    assert_eq!(
        bridge_wire_screen::state(&mut screen).await,
        json!({"state":"ready","message":"Echo Live ready"})
    );
    assert_eq!(host.command("pcm").await, "30720 0");
    bridge.finish(0).await;
    host.finish(0).await;
    bridge_wire_delegation::check_journey(&model, &spoof);
    check_wire_journey_trace(&path);
    assert_eq!(live.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(live.commentary.try_recv().is_err());
    spoof.shutdown().await.unwrap();
}
fn check_wire_journey_trace(path: &Path) {
    let entries = trace(path);
    assert_eq!(
        entries.iter().find(|e| e["event"] == "mic.frames").unwrap()["phase"],
        "first"
    );
    let expected = [
        "run.started",
        "mic.frames",
        "session.delegation.created",
        "tools/call",
        "session.commentary.append",
        "reply.started",
        "reply.played",
        "mic.frames",
    ];
    let edges: Vec<_> = entries
        .iter()
        .filter(|e| expected.contains(&e["event"].as_str().unwrap()))
        .collect();
    assert_eq!(
        edges.iter().map(|e| e["event"].as_str().unwrap()).collect::<Vec<_>>(),
        expected
    );
    for edge in edges {
        assert_eq!(edge["run_id"], "run-1");
        assert_eq!(edge["provider_auth_mode"], "api_key");
        if ["session.delegation.created", "tools/call", "session.commentary.append"]
            .contains(&edge["event"].as_str().unwrap())
        {
            assert_eq!(edge["delegation_id"], "d-1");
        }
    }
    let text = std::fs::read_to_string(path).unwrap();
    for private in ["synthetic-api-key", "synthetic-screen-pair", "synthetic-mcp-token"] {
        assert!(!text.contains(private), "trace disclosure");
    }
}
async fn wire_journey_screen(directory: &Path) -> Lines<BufReader<tokio::net::TcpStream>> {
    let address = trace(&directory.join("trace"))
        .iter()
        .find(|e| e["event"] == "screen.listening")
        .unwrap()["address"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut screen = bridge_wire_screen::stream(&address).await;
    assert_eq!(
        bridge_wire_screen::state(&mut screen).await,
        json!({"state":"ready","message":"Echo Live ready"})
    );
    bridge_wire_auth::invoke(
        directory,
        &["status", "--config", "bridge.toml"],
        0,
        "mode: api-key\naccount: n/a\nexpires: n/a\n",
    )
    .await;
    screen
}
async fn wire_journey_commentary(live: &mut bridge_peer::Live) {
    let commentary = tokio::time::timeout(Duration::from_secs(5), live.commentary.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&commentary).unwrap(),
        json!({"type":"session.commentary.append","delegation_id":"d-1","text":"delegated result"})
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while live.microphone_frames.load(std::sync::atomic::Ordering::SeqCst) < 32 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
