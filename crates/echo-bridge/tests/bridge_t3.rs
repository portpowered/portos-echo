#![cfg(test)]
//! T3: prebuilt binary, pinned hassmic-host and synthetic Live on IPv4 loopback.
use echo_live::fake::{FakeLive, FakeResponse, FakeScript};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
};

#[path = "bridge_support/bridge_peer.rs"]
mod bridge_peer;
#[path = "bridge_support/bridge_short.rs"]
mod bridge_short;
#[path = "bridge_support/bridge_spine_edges.rs"]
mod bridge_spine_edges;

struct Process {
    child: Child,
    input: Option<ChildStdin>,
    lines: Lines<BufReader<ChildStdout>>,
}

impl Process {
    fn spawn(command: &mut Command) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let lines = BufReader::new(child.stdout.take().unwrap()).lines();
        Self { child, input, lines }
    }
    async fn line(&mut self) -> String {
        let line = tokio::time::timeout(Duration::from_secs(12), self.lines.next_line())
            .await
            .unwrap()
            .unwrap();
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
    let shell = if cfg!(windows) {
        let result = std::process::Command::new("python")
            .arg(root().join("scripts/make-posix.py"))
            .output()
            .unwrap();
        assert!(result.status.success());
        PathBuf::from(String::from_utf8(result.stdout).unwrap().trim()).join("sh.exe")
    } else {
        "sh".into()
    };
    let mut command = Command::new(&shell);
    if cfg!(windows) {
        let inherited = std::env::var_os("PATH").unwrap();
        let paths = std::iter::once(shell.parent().unwrap().to_path_buf()).chain(std::env::split_paths(&inherited));
        command.env("PATH", std::env::join_paths(paths).unwrap());
    }
    command.arg("tests/bridge_host.sh").current_dir(root());
    command
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
        .write_all(format!("{method} {path} HTTP/1.1\r\nHost: local\r\n\r\n").as_bytes())
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
        json!({"status":"running","wyoming":"connected","live":"idle"})
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
    let mic = entries.iter().find(|e| e["event"] == "mic.frames").unwrap();
    assert_eq!(mic["count"], 32);
    assert_eq!(mic["input_samples"], 10240);
    assert_eq!(mic["tail_samples"], 0);
    assert_eq!(mic["dropped_chunks"], 0);
    let calls = observer.calls().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].frames.len(), 32);
    assert_eq!(host.command("pcm").await, "30720 0");
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
            "reply.started",
            "reply.played",
            "wyoming.mic_frames",
            "mic.frames",
            "wyoming.run_ended"
        ]
    );
    let wire = entries.iter().find(|e| e["event"] == "wyoming.mic_frames").unwrap();
    assert_eq!(wire["count"], mic["received_chunks"]);
    bridge.finish(0).await;
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
    host.finish(0).await;
    fake.shutdown().await;
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
