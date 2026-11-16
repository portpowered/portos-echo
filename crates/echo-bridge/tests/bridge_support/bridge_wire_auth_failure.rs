//! T3: configured prebuilt process retains health/screen after credential refusal.
use super::*;
use tokio::net::TcpListener;

pub(super) async fn wait_event(path: &Path, event: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let text = tokio::fs::read_to_string(path).await.unwrap();
            if text
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .any(|value| value["event"] == event)
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn wake(stream: &mut tokio::net::TcpStream) {
    bridge_short::send(
        stream,
        "run-pipeline",
        json!({
            "start_stage":"asr", "end_stage":"tts", "restart_on_end":false, "wake_word_name":"alexa"
        }),
    )
    .await;
}

async fn retained_failure(credential: Option<&str>, status: u16) {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    bridge_wire_screen::setup(directory.path(), listener.local_addr().unwrap().port());
    credential_file(directory.path(), credential);
    let fake = FakeLive::bind(FakeScript {
        events: vec![],
        response: FakeResponse::Http {
            status,
            body: "synthetic private body".into(),
            location: None,
        },
    })
    .await
    .unwrap();
    let observer = fake.observer();
    let origin = fake.base_url().trim_end_matches('/').to_owned();
    let mut bridge = Process::spawn(&mut bridge_wire_screen::run_command(directory.path(), &origin));
    let mut satellite = listener.accept().await.unwrap().0;
    bridge_short::register(&mut satellite, &mut bridge).await;
    let path = directory.path().join("trace");
    let relink = status == 401;
    wait_event(&path, if relink { "auth.relink_required" } else { "run.failed" }).await;
    let screen = assert_surfaces(&path, relink).await;
    if relink {
        assert_refused(directory.path(), &mut satellite, &path, &observer, credential).await;
    }
    bridge.finish(0).await;
    assert!(tokio::net::TcpStream::connect(&screen).await.is_err());
    assert_private_trace(&path);
    fake.shutdown().await;
}

async fn assert_refused(
    directory: &Path,
    satellite: &mut tokio::net::TcpStream,
    path: &Path,
    observer: &echo_live::fake::FakeObserver,
    credential: Option<&str>,
) {
    // Repairing the file cannot silently clear the in-process refusal latch.
    std::fs::write(directory.join("credential"), "synthetic-repaired-key").unwrap();
    wake(satellite).await;
    wait_event(path, "run.refused").await;
    assert_eq!(
        observer.calls().unwrap().len(),
        usize::from(credential.is_some_and(|s| !s.contains('\0')))
    );
}

fn credential_file(directory: &Path, credential: Option<&str>) {
    let file = directory.join("credential");
    if let Some(credential) = credential {
        std::fs::write(file, credential).unwrap();
    } else {
        std::fs::remove_file(file).unwrap();
    }
}

fn assert_private_trace(path: &Path) {
    let text = std::fs::read_to_string(path).unwrap();
    for private in ["synthetic-api-key", "synthetic-repaired-key", "synthetic private body"] {
        assert!(!text.contains(private), "private trace disclosure");
    }
    assert_eq!(trace(path).iter().filter(|e| e["event"] == "reply.played").count(), 0);
}

pub(super) async fn assert_surfaces(path: &Path, relink: bool) -> String {
    let entries = trace(path);
    let screen = entries.iter().find(|e| e["event"] == "screen.listening").unwrap()["address"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut frames = bridge_wire_screen::stream(&screen).await;
    let state = bridge_wire_screen::state(&mut frames).await;
    assert_eq!(state["state"], "error");
    assert_eq!(state["relink"], relink);
    if relink {
        relink_instruction(&screen, &mut frames, &state).await;
    }
    let response = health(entries[0]["health_addr"].as_str().unwrap(), "GET", "/healthz").await;
    assert_eq!(
        serde_json::from_str::<Value>(response.split_once("\r\n\r\n").unwrap().1).unwrap(),
        json!({"status":"running","wyoming":"connected","live":"error","relink_required":relink,"kill_switch":false})
    );
    screen
}

async fn relink_instruction(address: &str, frames: &mut Lines<BufReader<tokio::net::TcpStream>>, expected: &Value) {
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let body = r#"{"action":"relink"}"#;
    socket.write_all(format!(
        "POST /actions?token=synthetic-screen-pair HTTP/1.1\r\nHost: local\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
    ).as_bytes()).await.unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 204 No Content\r\n"));
    assert_eq!(bridge_wire_screen::state(frames).await, *expected);
    assert_eq!(
        expected["message"],
        "Run echo-bridge signin with your config, then restart the bridge."
    );
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_auth_failure_missing_invalid_and_401_latch_without_retry() {
    for credential in [None, Some("bad\0key"), Some("synthetic-api-key")] {
        retained_failure(credential, 401).await;
    }
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_auth_failure_outage_keeps_screen_without_claiming_relink() {
    retained_failure(Some("synthetic-api-key"), 503).await;
}

async fn auth_reply(listener: &TcpListener, path: &str, status: u16, reply: Value) {
    let mut stream = listener.accept().await.unwrap().0;
    let request = bridge_wire_auth::request(&mut stream).await;
    assert!(
        request.starts_with(&format!("POST {path} HTTP/1.1\r\n")),
        "auth endpoint mismatch"
    );
    let body = reply.to_string();
    stream
        .write_all(
            format!(
                "HTTP/1.1 {status} Result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
}

async fn recovered_run(directory: &Path, listener: &TcpListener, token: &str) {
    std::fs::rename(directory.join("trace"), directory.join("before-restart.trace")).unwrap();
    let live = FakeLive::bind(FakeScript {
        events: vec![json!({"type":"response.audio.done"}).to_string()],
        response: FakeResponse::Answer,
    })
    .await
    .unwrap();
    let origin = live.base_url().trim_end_matches('/').to_owned();
    let observer = live.observer();
    let mut bridge = Process::spawn(&mut bridge_wire_screen::run_command(directory, &origin));
    let mut satellite = listener.accept().await.unwrap().0;
    bridge_short::register(&mut satellite, &mut bridge).await;
    complete_recovered(&mut satellite, &mut bridge).await;
    assert_recovered(directory, &observer, token).await;
    bridge.finish(0).await;
    assert!(
        !std::fs::read_to_string(directory.join("trace"))
            .unwrap()
            .contains(token),
        "credential disclosure"
    );
    live.shutdown().await;
}

async fn complete_recovered(satellite: &mut tokio::net::TcpStream, bridge: &mut Process) {
    assert_eq!(bridge_short::read_reply(satellite).await.0["type"], "audio-start");
    assert_eq!(bridge_short::read_reply(satellite).await.0["type"], "audio-stop");
    bridge_short::send(satellite, "played", json!({})).await;
    bridge_short::send(satellite, "audio-stop", json!({})).await;
    assert_eq!(
        serde_json::from_str::<Value>(&bridge.line().await).unwrap(),
        json!({"event":"bridge.run_completed","run_id":"run-1"})
    );
}

async fn assert_recovered(directory: &Path, observer: &echo_live::fake::FakeObserver, token: &str) {
    let calls = observer.calls().unwrap();
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0]
            .request
            .header_matches("authorization", &format!("Bearer {token}")),
        "credential mismatch"
    );
    let entries = trace(&directory.join("trace"));
    let response = health(entries[0]["health_addr"].as_str().unwrap(), "GET", "/healthz").await;
    assert_eq!(
        serde_json::from_str::<Value>(response.split_once("\r\n\r\n").unwrap().1).unwrap()["relink_required"],
        false
    );
    assert_eq!(
        entries.iter().filter(|e| e["event"] == "auth.relink_required").count(),
        0
    );
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_auth_failure_revoked_refresh_latches_and_explicit_signin_restart_recovers() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    bridge_wire_screen::setup(directory.path(), listener.local_addr().unwrap().port());
    let auth = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = "http://127.0.0.1";
    let origin = format!("{base}:{}", auth.local_addr().unwrap().port());
    let config = directory.path().join("bridge.toml");
    let text = std::fs::read_to_string(&config)
        .unwrap()
        .replace("mode = \"api_key\"", "mode = \"chatgpt\"");
    std::fs::write(config, format!("{text}\n[chatgpt_auth]\nusercode_endpoint = '{origin}/user'\ndevice_token_endpoint = '{origin}/poll'\ntoken_endpoint = '{origin}/token'\nverification_url = '{origin}/verify'\n")).unwrap();
    std::fs::remove_file(directory.path().join("credential")).unwrap();
    bridge_wire_auth::prepare_configured_credential(directory.path(), "chatgpt").await;
    let path = directory.path().join("credential");
    let mut record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["expires"] = 1.into();
    record["last_refresh"] = 0.into();
    std::fs::write(&path, record.to_string()).unwrap();
    let (renewed, requests, task) = auth_recovery(auth);
    let mut bridge = Process::spawn(&mut bridge_wire_screen::run_command(directory.path(), &origin));
    let mut satellite = listener.accept().await.unwrap().0;
    bridge_short::register(&mut satellite, &mut bridge).await;
    let trace_path = directory.path().join("trace");
    wait_event(&trace_path, "auth.relink_required").await;
    assert_surfaces(&trace_path, true).await;
    wake(&mut satellite).await;
    wait_event(&trace_path, "run.refused").await;
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
    bridge.finish(0).await;
    bridge_wire_auth::invoke(
        directory.path(),
        &["signin", "--config", "bridge.toml"],
        0,
        &format!("code: TEST-CODE\nurl: {origin}/verify\nsigned in\n"),
    )
    .await;
    task.await.unwrap();
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 4);
    recovered_run(directory.path(), &listener, &renewed).await;
}

fn auth_recovery(
    listener: TcpListener,
) -> (
    String,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let token = format!("{}-renewed", bridge_wire_auth::token());
    let issued = token.clone();
    let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = requests.clone();
    let task = tokio::spawn(async move {
        for (path, status, reply) in [
            ("/token", 400, json!({"error":"invalid_grant"})),
            (
                "/user",
                200,
                json!({"device_auth_id":"synthetic-device", "user_code":"TEST-CODE", "interval":1}),
            ),
            (
                "/poll",
                200,
                json!({"authorization_code":"synthetic-code", "code_verifier":"synthetic-verifier"}),
            ),
            (
                "/token",
                200,
                json!({"access_token":issued,"refresh_token":"synthetic-renewed-refresh"}),
            ),
        ] {
            auth_reply(&listener, path, status, reply).await;
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    });
    (token, requests, task)
}
