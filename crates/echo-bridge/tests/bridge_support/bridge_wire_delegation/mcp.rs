//! T3 grant/control witnesses through the prebuilt bridge and unchanged auth fixture.
//! Resource admission and the milestone scenario are separate retained witnesses.
use super::*;
use echo_mcp_auth::mock::Mock;
use journey::{assert_restart_deadline, fresh_probe};
use std::{future::Future, task::Poll, time::Instant};
use strict::{observe_restart, wait_probe};
#[path = "mcp_failures.rs"]
mod faults;
#[path = "mcp_journey.rs"]
mod journey;
#[path = "mcp_strict.rs"]
mod strict;
struct Session {
    process: Process,
    satellite: TcpStream,
    screen: Lines<BufReader<TcpStream>>,
    control: String,
    wall_epoch: (u64, u64),
    trace_path: PathBuf,
    probe_schedule: Option<faults::ProbeSchedule>,
    wyoming_listener: TcpListener,
}
impl Session {
    async fn start(directory: &Path, mock: &Mock, first: bool) -> Self {
        Self::start_with(directory, mock, first, None).await
    }
    async fn start_with(directory: &Path, mock: &Mock, first: bool, peers: Option<(&str, &str)>) -> Self {
        Self::start_resource(directory, &mock.base, first, peers, None).await
    }
    async fn start_resource(
        directory: &Path,
        resource: &str,
        first: bool,
        peers: Option<(&str, &str)>,
        epoch: Option<u64>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        configure(directory, resource, first, listener.local_addr().unwrap().port());
        let path = directory.join("bridge.toml");
        if peers.is_some() && first {
            let text = std::fs::read_to_string(&path)
                .unwrap()
                .replace("[agent]", "[agent]\nmodel = 'fixture-model'");
            std::fs::write(&path, text).unwrap();
        }
        let mut command = clock_command(directory, resource, peers, epoch);
        let before = wall_now();
        let mut process = Process::spawn(&mut command);
        let mut satellite = listener.accept().await.unwrap().0;
        assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "run-satellite");
        assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "describe");
        bridge_short::send(&mut satellite, "info", json!({"satellite":{"name":"synthetic"}})).await;
        assert_eq!(process.line().await, r#"{"event":"bridge.ready"}"#);
        let wall_epoch = epoch.map_or_else(|| (before, wall_now()), |epoch| (epoch, epoch));
        let events = trace(&directory.join("trace"));
        let address = |event: &str| {
            events.iter().rev().find(|e| e["event"] == event).unwrap()["address"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let control = address("mcp.operator.listening");
        let screen_address = address("screen.listening");
        let text = std::fs::read_to_string(&path).unwrap().replace(
            "operator_listen = '127.0.0.1:0'",
            &format!("operator_listen = '{control}'"),
        );
        std::fs::write(path, text).unwrap();
        let mut screen = bridge_wire_screen::stream(&screen_address).await;
        assert_eq!(
            bridge_wire_screen::state(&mut screen).await,
            json!({"state":"ready","message":"Echo Live ready"})
        );
        Self {
            process,
            satellite,
            screen,
            control,
            wall_epoch,
            trace_path: directory.join("trace"),
            probe_schedule: None,
            wyoming_listener: listener,
        }
    }
    async fn advance(&mut self, milliseconds: u64) {
        let text = json!({"action":"advance_clock","milliseconds":milliseconds}).to_string();
        let line = self.process.command(&text).await;
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap(),
            json!({"event":"bridge.clock_advanced","milliseconds":milliseconds})
        );
        self.observe_held_advance().await;
    }
    async fn prompt(&mut self, mock: &Mock) {
        let state = bridge_wire_screen::state(&mut self.screen).await;
        assert_eq!(state["state"], "link_code");
        assert_eq!(state["user_code"], "TEST-CODE");
        assert_eq!(state["verification_uri"], format!("{}/approve", mock.base));
        let expiry = state["expires_at_unix"].as_u64().unwrap();
        // Normal mode samples wall UTC during startup, not when this observer
        // later receives the screen. Bound that sample independently; exact
        // fixture-epoch assertions live in the strict guarded journey.
        assert!((self.wall_epoch.0 + 1800..=self.wall_epoch.1 + 1800).contains(&expiry));
        assert_eq!(bridge_short::read(&mut self.satellite).await.0["type"], "describe");
        self.answer_probe().await;
    }
    #[allow(clippy::disallowed_methods, reason = "Host time for test")]
    async fn answer_recovery_probe(&mut self) {
        let cursor = std::fs::read(&self.trace_path).unwrap().len();
        self.send_probe_reply().await;
        strict::wait_records(
            || tokio::fs::read(&self.trace_path),
            cursor,
            true,
            Instant::now() + OBSERVATION_TIMEOUT,
        )
        .await;
    }
    async fn answer_probe(&mut self) {
        // Receiving this describe orders the cursor after the preceding probe's
        // flushed success. Each Session owns one child and one trace generation.
        let cursor = std::fs::read(&self.trace_path).unwrap().len();
        self.send_probe_reply().await;
        self.release_at_probe_wait(cursor).await;
        wait_probe(&self.trace_path, cursor).await;
    }
}
impl Session {
    #[allow(clippy::disallowed_methods, reason = "Host time for test")]
    async fn restart_probe(&mut self) {
        let deadline = Instant::now() + OBSERVATION_TIMEOUT;
        let cursor = std::fs::read(&self.trace_path).unwrap().len();
        assert!(
            fresh_probe(&std::fs::read(&self.trace_path).unwrap(), 0),
            "retained old-process success required"
        );
        let describe = observe_restart(
            bridge_short::read(&mut self.satellite),
            deadline,
            "missing describe",
            cursor,
        )
        .await
        .unwrap();
        assert_eq!(describe.0["type"], "describe");
        self.probe_schedule = Some(faults::ProbeSchedule::Held);
        observe_restart(self.answer_probe(), deadline, "fresh complete idle probe", cursor)
            .await
            .unwrap();
        assert_eq!(self.probe_schedule, Some(faults::ProbeSchedule::Waiting));
        let bytes = std::fs::read(&self.trace_path).unwrap();
        assert!(fresh_probe(&bytes, cursor));
        let records: Vec<Value> = serde_json::Deserializer::from_slice(&bytes[cursor..])
            .into_iter()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            records.iter().find(|r| r["event"] == "wyoming.probe_ok").unwrap()["elapsed_ms"],
            0
        );
        assert!(!records.iter().any(|r| r["event"] == "wyoming.reconnecting"));
    }
}
#[tokio::test(start_paused = true)]
#[allow(clippy::disallowed_methods, reason = "Host time for test")]
async fn restart_probe_observation_deadline() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    std::fs::write(&path, b"retained old process\n").unwrap();
    let cursor = std::fs::read(&path).unwrap().len();
    let deadline = Instant::now();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let mut socket = listener.accept().await.unwrap().0;
    assert_restart_deadline(bridge_short::read(&mut socket), deadline, "missing describe", cursor).await;
    assert_restart_deadline(wait_probe(&path, cursor), deadline, "fresh complete idle probe", cursor).await;
}
fn configure(directory: &Path, resource: &str, first: bool, port: u16) {
    if first {
        bridge_wire_screen::setup(directory, port);
    }
    let path = directory.join("bridge.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    let text = text
        .lines()
        .map(|line| {
            if line.starts_with("wyoming_port =") {
                format!("wyoming_port = {port}")
            } else if line.starts_with("operator_listen =") {
                "operator_listen = '127.0.0.1:0'".into()
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let text = if first {
        let state = directory.join("portos").to_str().unwrap().replace('\\', "/");
        let mcp = format!("[mcp]\nstate_dir = '{state}'\nclient_id = 'echo-lab'\noperator_listen = '127.0.0.1:0'\n");
        text.replace("[[mcp.server]]", &format!("{mcp}[[mcp.server]]"))
            .replace("http://127.0.0.1:18080/mcp", &format!("{resource}/mcp"))
            .replace("auth = \"none\"", "auth = 'portos_device_code'")
    } else {
        text
    };
    std::fs::write(&path, text).unwrap();
}
async fn drive<F: Future>(future: F) -> F::Output {
    tokio::pin!(future);
    std::future::poll_fn(|cx| match future.as_mut().poll(cx) {
        Poll::Ready(value) => Poll::Ready(value),
        Poll::Pending => {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await
}
fn operator(directory: &Path, verb: &str) -> tokio::task::JoinHandle<std::process::Output> {
    let mut command = bridge_wire_auth::command(directory, &[verb, "--config", "bridge.toml"]);
    tokio::spawn(async move { command.output().await.unwrap() })
}
#[track_caller]
fn completed(output: &std::process::Output, code: i32, stdout: &str, stderr: &str) {
    assert_eq!(
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        (Some(code), stdout.into(), stderr.into()),
        "operator result (exit code, stdout, stderr)"
    );
}
fn record(directory: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(directory.join("portos/grant.json")).unwrap()).unwrap()
}
fn linked_output(issuer: &str) -> String {
    format!("{}MCP linked\n", link_guidance(issuer))
}
fn link_guidance(issuer: &str) -> String {
    format!("Verification URL: {issuer}/approve\nUser code: TEST-CODE\n")
}
#[tokio::test(start_paused = true)]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn mcp_link_cli_screen_rotation_reload_and_healthy_unlink() {
    drive(async {
        let directory = tempfile::tempdir().unwrap();
        let mock = Mock::bind().await.unwrap();
        let mut session = Session::start(directory.path(), &mock, true).await;
        let link = operator(directory.path(), "link");
        session.prompt(&mock).await;
        tokio::time::advance(Duration::from_secs(5)).await;
        session.advance(5000).await;
        completed(&link.await.unwrap(), 0, &linked_output(&mock.base), "");
        assert_eq!(bridge_wire_screen::state(&mut session.screen).await, json!({"state":"ready","message":"Echo Live ready"}));
        let first = record(directory.path());
        for generation in [2, 3] {
            completed(&operator(directory.path(), "--refresh-now").await.unwrap(), 0, "MCP refreshed\n", "");
            let current = record(directory.path());
            assert_eq!(current["grant"]["refresh_token"], format!("refresh-{generation}"));
            assert!(current["private_key_jwk"] == first["private_key_jwk"], "DPoP key changed");
        }
        session.process.finish(0).await;
        drop(session.satellite);
        let mut restarted = Session::start(directory.path(), &mock, false).await;
        assert_eq!(record(directory.path())["grant"]["refresh_token"], "refresh-3");
        completed(&operator(directory.path(), "unlink").await.unwrap(), 0, "MCP unlinked\n", "");
        assert_eq!(bridge_wire_screen::state(&mut restarted.screen).await, json!({"state":"error","message":"Run echo-bridge link with your config to relink Port OS MCP.","relink":true}));
        completed(&operator(directory.path(), "unlink").await.unwrap(), 0, "MCP unlinked\n", "");
        assert_eq!(record(directory.path())["grant"], Value::Null);
        {
        let state = mock.state.lock().unwrap();
        assert_eq!(state.requests.iter().filter(|r| r.path == "/custom/link").count(), 1);
        assert_eq!(state.requests.iter().filter(|r| r.path == "/custom/end").count(), 1);
        assert_eq!(state.requests.iter().filter(|r| r.form.get("grant_type").is_some_and(|v| v == "refresh_token")).count(), 2);
        assert!(state.requests.iter().filter(|r| r.path == "/custom/end").all(|r| !r.headers.contains_key("dpop")));
        }
        restarted.process.finish(0).await;
        let lifecycle: Vec<_> = trace(&directory.path().join("trace")).into_iter()
            .filter(|e| matches!(e["event"].as_str(), Some("mcp.link.started" | "mcp.link.approved" | "mcp.refresh.committed" | "mcp.unlink.completed" | "mcp.grant.reloaded")))
            .map(|e| json!({"event":e["event"],"operation_id":e["operation_id"]})).collect();
        assert_eq!(lifecycle, json!([
            {"event":"mcp.link.started","operation_id":1},
            {"event":"mcp.link.approved","operation_id":1},
            {"event":"mcp.refresh.committed","operation_id":2},
            {"event":"mcp.refresh.committed","operation_id":3},
            {"event":"mcp.unlink.completed","operation_id":1},
            {"event":"mcp.unlink.completed","operation_id":2}
        ]).as_array().unwrap().clone());
        let text = std::fs::read_to_string(directory.path().join("trace")).unwrap();
        for private in ["access-1", "refresh-3", "synthetic-device", "synthetic-screen-pair"] { assert!(!text.contains(private)); }
    }).await;
}
#[tokio::test(start_paused = true)]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn mcp_link_crash_after_rotation_reloads_durable_generation_before_use() {
    drive(async {
        let directory = tempfile::tempdir().unwrap();
        // Deliberately skewed from wall UTC: neither startup may resample it.
        // The mock and both children share this independent oracle, not a proof.
        let epoch = 1_800_000_000;
        let mock = Mock::bind_at_epoch(epoch).await.unwrap();
        let mut session = Session::start_resource(directory.path(), &mock.base, true, None, Some(epoch)).await;
        let link = operator(directory.path(), "link");
        session.prompt(&mock).await;
        tokio::time::advance(Duration::from_secs(5)).await;
        session.advance(5000).await;
        completed(&link.await.unwrap(), 0, &linked_output(&mock.base), "");
        let initial = record(directory.path());
        completed(
            &operator(directory.path(), "--refresh-now").await.unwrap(),
            0,
            "MCP refreshed\n",
            "",
        );
        let rotated = record(directory.path());
        assert_eq!(rotated["grant"]["refresh_token"], "refresh-2");
        assert_eq!(rotated["private_key_jwk"], initial["private_key_jwk"]);
        // CLI completion is the public post-persistence barrier. No wake has
        // occurred: terminate abruptly before any MCP use, without EOF cleanup.
        session.process.child.kill().await.unwrap();
        assert!(!session.process.child.wait().await.unwrap().success());
        drop(session);
        assert_eq!(record(directory.path()), rotated);
        let mut restarted = Session::start_resource(directory.path(), &mock.base, false, None, Some(epoch)).await;
        // Resume the shared epoch at the mock's elapsed fake time. Restart
        // latency must not enter the proof timestamp through a new wall sample.
        restarted.restart_probe().await;
        restarted.advance(5000).await;
        completed(
            &operator(directory.path(), "--refresh-now").await.unwrap(),
            0,
            "MCP refreshed\n",
            "",
        );
        let recovered = record(directory.path());
        assert_eq!(recovered["grant"]["refresh_token"], "refresh-3");
        assert_eq!(recovered["private_key_jwk"], initial["private_key_jwk"]);
        assert_crash_requests(&mock);
        restarted.process.finish(0).await;
        strict::assert_no_restart_disconnect(&restarted.trace_path);
    })
    .await;
}
fn assert_crash_requests(mock: &Mock) {
    let state = mock.state.lock().unwrap();
    assert_eq!(state.requests.iter().filter(|r| r.path == "/custom/link").count(), 1);
    let refreshes: Vec<_> = state
        .requests
        .iter()
        .filter(|r| r.form.get("grant_type").map(String::as_str) == Some("refresh_token"))
        .map(|r| r.form["refresh_token"].as_str())
        .collect();
    assert_eq!(refreshes, ["refresh-1", "refresh-2"]);
    assert_eq!(state.requests.iter().filter(|r| r.path == "/mcp").count(), 0);
    assert!(!state.revoked);
}
#[tokio::test(start_paused = true)]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn mcp_link_denial_and_expiry_are_fixed_and_stop_polling() {
    drive(async {
        for (category, message) in [
            ("access_denied", "Link approval denied"),
            ("expired_token", "Link code expired"),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let mock = Mock::bind().await.unwrap();
            mock.state.lock().unwrap().polling.push_back(category.into());
            let mut session = Session::start(directory.path(), &mock, true).await;
            let link = operator(directory.path(), "link");
            session.prompt(&mock).await;
            tokio::time::advance(Duration::from_secs(5)).await;
            session.advance(5000).await;
            completed(
                &link.await.unwrap(),
                1,
                &link_guidance(&mock.base),
                &format!("echo-bridge: {message}\n"),
            );
            assert_eq!(
                bridge_wire_screen::state(&mut session.screen).await,
                json!({"state":"error","message":message,"relink":true})
            );
            assert_eq!(record(directory.path())["grant"], Value::Null);
            session.advance(60000).await;
            assert_eq!(
                mock.state
                    .lock()
                    .unwrap()
                    .requests
                    .iter()
                    .filter(|r| r.path == "/custom/token")
                    .count(),
                1
            );
            session.process.finish(0).await;
        }
    })
    .await;
}
#[tokio::test(start_paused = true)]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn mcp_link_control_auth_busy_and_cancellation_do_not_duplicate_mutations() {
    drive(async {
        let directory = tempfile::tempdir().unwrap();
        let mock = Mock::bind().await.unwrap();
        let mut session = Session::start(directory.path(), &mock, true).await;
        let http = reqwest::Client::new();
        let base = "http://127.0.0.1";
        let port = session.control.parse::<std::net::SocketAddr>().unwrap().port();
        let url = format!("{base}:{port}/operator/mcp");
        for (token, origin, expected) in [("wrong", false, 401), ("synthetic-screen-pair", true, 403)] {
            let mut request = http.post(&url).bearer_auth(token).json(&json!({"operation":"link"}));
            if origin { request = request.header("Origin", base); }
            assert_eq!(request.send().await.unwrap().status().as_u16(), expected);
        }
        let pending = http.post(&url).bearer_auth("synthetic-screen-pair").json(&json!({"operation":"link"})).send();
        let pending = tokio::spawn(pending);
        session.prompt(&mock).await;
        let response = http.post(&url).bearer_auth("synthetic-screen-pair").json(&json!({"operation":"unlink"})).send().await.unwrap();
        assert_eq!(response.status().as_u16(), 409);
        pending.abort();
        let _ = pending.await;
        assert_eq!(bridge_wire_screen::state(&mut session.screen).await, json!({"state":"error","message":"Run echo-bridge link with your config to relink Port OS MCP.","relink":true}));
        session.advance(5000).await;
        {
        let state = mock.state.lock().unwrap();
        assert_eq!(state.requests.iter().filter(|r| r.path == "/custom/link").count(), 1);
        assert_eq!(state.requests.iter().filter(|r| r.path == "/custom/token").count(), 0);
        assert_eq!(state.requests.iter().filter(|r| r.path == "/custom/end").count(), 0);
        }
        session.process.finish(0).await;
    }).await;
}
fn clock_command(directory: &Path, resource: &str, peers: Option<(&str, &str)>, epoch: Option<u64>) -> Command {
    let mut command = match peers {
        Some((live, model)) => run_command(directory, live, model),
        None => bridge_wire_screen::run_command(directory, resource),
    };
    command.arg("--sim-clock");
    if let Some(epoch) = epoch {
        command.args(["--sim-epoch", &epoch.to_string()]);
    }
    command
}
#[allow(clippy::disallowed_methods, reason = "Host time for test")]
fn wall_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
#[tokio::test(start_paused = true)]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn idle_probe_success_is_once_per_completion_and_pending_eof_is_not_success() {
    drive(async {
        let directory = tempfile::tempdir().unwrap();
        let mock = Mock::bind().await.unwrap();
        let mut session = Session::start(directory.path(), &mock, true).await;
        for count in 1..=2 {
            assert_eq!(bridge_short::read(&mut session.satellite).await.0["type"], "describe");
            let cursor = std::fs::read(&session.trace_path).unwrap().len();
            bridge_short::send(
                &mut session.satellite,
                "info",
                json!({"satellite":{"name":"synthetic"}}),
            )
            .await;
            wait_probe(&session.trace_path, cursor).await;
            let records = trace(&session.trace_path);
            assert_eq!(
                records.iter().filter(|r| r["event"] == "wyoming.probe_ok").count(),
                count
            );
            assert_eq!(records.iter().filter(|r| r["event"] == "wyoming.connected").count(), 1);
            session.advance(100).await;
        }
        // The third describe is genuinely outstanding when EOF cancels its owner.
        assert_eq!(bridge_short::read(&mut session.satellite).await.0["type"], "describe");
        session.process.finish(0).await;
        let records = trace(&session.trace_path);
        assert_eq!(records.iter().filter(|r| r["event"] == "wyoming.probe_ok").count(), 2);
        assert_eq!(
            records.iter().filter(|r| r["event"] == "wyoming.reconnecting").count(),
            0
        );
        let mut byte = [0];
        assert_eq!(session.satellite.read(&mut byte).await.unwrap(), 0);
        session.process.assert_collected();
    })
    .await;
}
