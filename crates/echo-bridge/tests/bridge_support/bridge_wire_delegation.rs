//! Component T3: delivered process, existing delegation/model/MCP implementations.
//! These controlled Wyoming/Live cases do not substitute for the pinned-host journey.
use super::*;
use echo_contracts::secret::Secret;
use echo_spoof_mcp::{Options, Spoof};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::net::{TcpListener, TcpStream};

#[path = "bridge_wire_delegation/failures.rs"]
mod failures;
#[path = "bridge_wire_delegation/kill.rs"]
mod kill;
#[path = "bridge_wire_delegation/mcp.rs"]
mod mcp;
#[path = "bridge_wire_delegation/runtime.rs"]
mod runtime;

struct Request {
    start: String,
    headers: HashMap<String, String>,
    body: Value,
}

pub(super) struct Model {
    pub(super) origin: String,
    requests: Arc<Mutex<Vec<Request>>>,
    worker: tokio::task::JoinHandle<()>,
}

impl Drop for Model {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

impl Model {
    pub(super) async fn start(outage: bool, allowed: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = "http://127.0.0.1";
        let origin = format!("{base}:{}", listener.local_addr().unwrap().port());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = requests.clone();
        let worker = tokio::spawn(async move {
            loop {
                let mut socket = listener.accept().await.unwrap().0;
                let request = read(&mut socket).await;
                let index = observed.lock().unwrap().len();
                let item = if index == 0 {
                    json!({"type":"function_call","call_id":"call-1","name":"tool_0","arguments":"{\"text\":\"delegated echo\"}"})
                } else {
                    json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":if allowed { "delegated result" } else { "tool denied" }}]})
                };
                observed.lock().unwrap().push(request);
                let body = if outage {
                    "synthetic private provider body".into()
                } else {
                    sse(&item)
                };
                let status = if outage { 503 } else { 200 };
                let response = format!(
                    "HTTP/1.1 {status} fixture\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        Self {
            origin,
            requests,
            worker,
        }
    }
}

fn sse(item: &Value) -> String {
    format!(
        "data: {}\n\ndata: {}\n\n",
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        json!({"type":"response.completed","response":{"status":"completed"}})
    )
}

async fn read(socket: &mut TcpStream) -> Request {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        assert!(head.len() < 16384);
        head.push(socket.read_u8().await.unwrap());
    }
    let head = String::from_utf8(head).unwrap();
    let mut lines = head.lines();
    let start = lines.next().unwrap().to_owned();
    let headers: HashMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.to_ascii_lowercase(), value.trim().into()))
        .collect();
    let size: usize = headers["content-length"].parse().unwrap();
    assert!(size < 65536);
    let mut bytes = vec![0; size];
    socket.read_exact(&mut bytes).await.unwrap();
    Request {
        start,
        headers,
        body: serde_json::from_slice(&bytes).unwrap(),
    }
}

pub(super) fn event(id: &str, target: &str) -> String {
    json!({"type":"session.delegation.created","delegation":{"id":id,"target":target,"input":"echo"}}).to_string()
}

pub(super) async fn configure(directory: &Path, port: u16, mode: &str, spoof: &Spoof, allowed: bool) {
    bridge_wire_screen::setup(directory, port);
    let path = directory.join("bridge.toml");
    let content = std::fs::read_to_string(&path)
        .unwrap()
        .replace("mode = \"api_key\"", &format!("mode = \"{mode}\""))
        .replace("[agent]", "[agent]\nmodel = 'fixture-model'")
        .replace("http://127.0.0.1:18080/mcp", spoof.url())
        .replace("auth = \"none\"", "auth = 'bearer_file'\nbearer_file = 'mcp-token'");
    let content = if allowed {
        content
    } else {
        content.replace("allow_tools = [\"test.echo\"]", "allow_tools = []")
    };
    // Configured override mode validates potential refresh as loopback too.
    let content = format!("{content}\n[chatgpt_auth]\ntoken_endpoint = 'http://127.0.0.1:1/token'\n");
    std::fs::write(path, content).unwrap();
    std::fs::write(directory.join("mcp-token"), "synthetic-mcp-token\r\n").unwrap();
    if mode == "chatgpt" {
        std::fs::remove_file(directory.join("credential")).unwrap();
    }
    bridge_wire_auth::prepare_configured_credential(directory, mode).await;
}

async fn commentary(observer: &echo_live::fake::FakeObserver) -> Value {
    tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        loop {
            let calls = observer.calls().unwrap();
            if let Some(text) = calls.first().and_then(|call| call.commentary.first()) {
                return serde_json::from_str(text).unwrap();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

async fn delegated(mode: &str, allowed: bool, outage: bool) {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let spoof = Spoof::start(Options {
        expected_bearer: Some(Secret::new("synthetic-mcp-token".into())),
        ..Options::default()
    })
    .await
    .unwrap();
    configure(
        directory.path(),
        listener.local_addr().unwrap().port(),
        mode,
        &spoof,
        allowed,
    )
    .await;
    let renewal = bridge_wire_auth_failure::rotate_credential(directory.path(), mode).await;
    let model = Model::start(outage, allowed).await;
    let fake = FakeLive::bind(FakeScript {
        events: vec![
            event("d-1", "client"),
            event("d-1", "client"),
            event("d-responses", "responses"),
        ],
        response: FakeResponse::Answer,
    })
    .await
    .unwrap();
    let observer = fake.observer();
    let origin = fake.base_url().trim_end_matches('/').to_owned();
    let mut command = run_command(directory.path(), &origin, &model.origin);
    let mut bridge = Process::spawn(&mut command);
    let mut satellite = listener.accept().await.unwrap().0;
    bridge_short::register(&mut satellite, &mut bridge).await;
    let _satellite = bridge_short::Satellite::new(satellite);
    assert_commentary(&observer, allowed, outage).await;
    let path = directory.path().join("trace");
    bridge_wire_auth_failure::wait_event(&path, "session.commentary.append").await;
    bridge.finish(0).await;
    bridge_wire_auth_failure::check_rotation(directory.path(), renewal).await;
    check(&path, &model, &observer, &spoof, mode, allowed, outage);
    fake.shutdown().await;
    spoof.shutdown().await.unwrap();
}

async fn assert_commentary(observer: &echo_live::fake::FakeObserver, allowed: bool, outage: bool) {
    let delivered = commentary(observer).await;
    let expected = expected(allowed, outage);
    assert_eq!(
        delivered,
        json!({"type":"session.commentary.append","delegation_id":"d-1","text":expected})
    );
}

fn expected(allowed: bool, outage: bool) -> &'static str {
    if outage {
        "model request failed"
    } else if allowed {
        "delegated result"
    } else {
        "tool denied"
    }
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_delegation_lost_model_credential_latches_relink_and_cuts() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let spoof = Spoof::start(Options::default()).await.unwrap();
    configure(
        directory.path(),
        listener.local_addr().unwrap().port(),
        "api_key",
        &spoof,
        true,
    )
    .await;
    let model = Model::start(false, true).await;
    let (ready, entered) = tokio::sync::oneshot::channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    let live = bridge_peer::Live::bind_after(0, Some((ready, wait))).await;
    let mut bridge = Process::spawn(&mut run_command(directory.path(), &live.url, &model.origin));
    let mut satellite = listener.accept().await.unwrap().0;
    bridge_short::register(&mut satellite, &mut bridge).await;
    let mut satellite = bridge_short::Satellite::new(satellite);
    // Live has consumed the valid key; fail the model's separate acquisition.
    entered.await.unwrap();
    std::fs::remove_file(directory.path().join("credential")).unwrap();
    release.send(()).unwrap();
    assert_eq!(bridge_short::read_reply(&mut satellite).await.0["type"], "audio-start");
    live.events.send(event("d-auth", "client")).await.unwrap();
    assert_eq!(bridge_short::read_reply(&mut satellite).await.0["type"], "audio-stop");
    bridge_short::send(&mut satellite, "played", json!({})).await;
    bridge_short::send(&mut satellite, "audio-stop", json!({})).await;
    let path = directory.path().join("trace");
    bridge_wire_auth_failure::wait_event(&path, "auth.relink_required").await;
    bridge_wire_auth_failure::assert_surfaces(&path, true).await;
    failures::assert_relink_error(&mut satellite).await;
    // File repair is insufficient: another wake must refuse without dispatch.
    std::fs::write(directory.path().join("credential"), "synthetic-repaired-key").unwrap();
    bridge_short::send(
        &mut satellite,
        "run-pipeline",
        json!({
            "start_stage":"asr", "end_stage":"tts", "restart_on_end":false, "wake_word_name":"alexa"
        }),
    )
    .await;
    bridge_wire_auth_failure::wait_event(&path, "run.refused").await;
    failures::assert_relink_error(&mut satellite).await;
    assert_eq!(live.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(model.requests.lock().unwrap().len(), 0);
    assert_eq!(spoof.snapshot().unwrap().calls.len(), 0);
    bridge.finish(0).await;
    let remaining = bridge_short::read_to_close(&mut satellite).await;
    assert_eq!(remaining, Vec::<u8>::new()); // No duplicate error, audio or replay after the two refusals.
    failures::check_credential_refusals(&path);
    spoof.shutdown().await.unwrap();
}

pub(super) fn run_command(directory: &Path, origin: &str, model: &str) -> Command {
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
            model,
        ],
    )
}

fn check(
    path: &Path,
    model: &Model,
    observer: &echo_live::fake::FakeObserver,
    spoof: &Spoof,
    mode: &str,
    allowed: bool,
    outage: bool,
) {
    let calls = observer.calls().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].commentary.len(), 1);
    let token = if mode == "chatgpt" {
        format!("{}-rotated", bridge_wire_auth::token())
    } else {
        "synthetic-api-key".into()
    };
    assert!(
        calls[0]
            .request
            .header_matches("authorization", &format!("Bearer {token}")),
        "Live credential mismatch"
    );
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), if outage { 1 } else { 2 });
    check_headers(&requests, mode, &token);
    check_tools(&requests, spoof, allowed, outage);
    check_trace(path, outage, &token);
}

fn check_headers(requests: &[Request], mode: &str, token: &str) {
    for request in requests {
        assert!(
            request
                .headers
                .get("authorization")
                .is_some_and(|value| value == &format!("Bearer {token}")),
            "model credential mismatch"
        );
        assert_eq!(request.headers.contains_key("chatgpt-account-id"), mode == "chatgpt");
        if mode == "chatgpt" {
            assert!(
                request
                    .headers
                    .get("chatgpt-account-id")
                    .is_some_and(|value| value == "synthetic-account-1234"),
                "model account mismatch"
            );
        }
        assert_eq!(
            request.start,
            if mode == "chatgpt" {
                "POST /backend-api/codex/responses HTTP/1.1"
            } else {
                "POST /v1/responses HTTP/1.1"
            }
        );
        assert_eq!(request.body["model"], "fixture-model");
    }
}

fn check_tools(requests: &[Request], spoof: &Spoof, allowed: bool, outage: bool) {
    let snapshot = spoof.snapshot().unwrap();
    assert_eq!(snapshot.calls.len(), usize::from(allowed && !outage));
    if allowed && !outage {
        assert_eq!(snapshot.calls[0].name, "test.echo");
        assert_eq!(snapshot.calls[0].args, json!({"text":"delegated echo"}));
    }
    if !outage {
        let result: Value = serde_json::from_str(requests[1].body["input"][3]["output"].as_str().unwrap()).unwrap();
        assert_eq!(
            result,
            if allowed {
                json!({"content":[{"type":"text","text":"delegated echo"}],"is_error":false})
            } else {
                json!({"content":{"error":"denied"},"is_error":true})
            }
        );
    }
}

fn check_trace(path: &Path, outage: bool, token: &str) {
    let entries = trace(path);
    let edges: Vec<_> = entries
        .iter()
        .filter(|e| {
            ["session.delegation.created", "tools/call", "session.commentary.append"]
                .contains(&e["event"].as_str().unwrap())
        })
        .collect();
    assert_eq!(edges.len(), if outage { 2 } else { 3 });
    for edge in &edges {
        assert_eq!(
            (&edge["run_id"], &edge["delegation_id"]),
            (&json!("run-1"), &json!("d-1"))
        );
    }
    assert_eq!(edges[0]["event"], "session.delegation.created");
    assert_eq!(edges.last().unwrap()["event"], "session.commentary.append");
    let text = std::fs::read_to_string(path).unwrap();
    let original = bridge_wire_auth::token();
    for private in [
        token,
        original.as_str(),
        "synthetic-refresh",
        "synthetic-rotated-refresh",
        "synthetic-account-1234",
        "synthetic-mcp-token",
        "synthetic private provider body",
    ] {
        assert!(!text.contains(private), "trace disclosure");
    }
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_delegation_credentials_duplicate_target_and_exact_tool_result() {
    for mode in ["chatgpt", "api_key"] {
        delegated(mode, true, false).await;
    }
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_delegation_denial_and_model_outage_do_not_replay() {
    delegated("api_key", false, false).await;
    delegated("api_key", true, true).await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_delegation_eof_cancels_inflight_model_without_commentary() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = "http://127.0.0.1";
    let origin = format!("{base}:{}", model.local_addr().unwrap().port());
    let spoof = Spoof::start(Options {
        expected_bearer: Some(Secret::new("synthetic-mcp-token".into())),
        ..Options::default()
    })
    .await
    .unwrap();
    configure(
        directory.path(),
        listener.local_addr().unwrap().port(),
        "api_key",
        &spoof,
        true,
    )
    .await;
    let live = FakeLive::bind(FakeScript {
        events: vec![event("d-stalled", "client")],
        response: FakeResponse::Answer,
    })
    .await
    .unwrap();
    let live_origin = live.base_url().trim_end_matches('/').to_owned();
    let mut bridge = Process::spawn(&mut run_command(directory.path(), &live_origin, &origin));
    let mut satellite = listener.accept().await.unwrap().0;
    bridge_short::register(&mut satellite, &mut bridge).await;
    let _satellite = bridge_short::Satellite::new(satellite);
    let mut socket = tokio::time::timeout(OBSERVATION_TIMEOUT, model.accept())
        .await
        .unwrap()
        .unwrap()
        .0;
    assert_eq!(read(&mut socket).await.body["model"], "fixture-model");
    bridge.finish(0).await;
    assert_eq!(
        tokio::time::timeout(OBSERVATION_TIMEOUT, socket.read(&mut [0]))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert_eq!(live.observer().calls().unwrap()[0].commentary.len(), 0);
    assert_eq!(spoof.snapshot().unwrap().calls.len(), 0);
    let path = directory.path().join("trace");
    assert_eq!(
        trace(&path)
            .iter()
            .filter(|e| e["event"] == "session.commentary.append")
            .count(),
        0
    );
    live.shutdown().await;
    spoof.shutdown().await.unwrap();
}

pub(super) fn check_journey(model: &Model, spoof: &Spoof) {
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    check_headers(&requests, "api_key", "synthetic-api-key");
    check_tools(&requests, spoof, true, false);
}
