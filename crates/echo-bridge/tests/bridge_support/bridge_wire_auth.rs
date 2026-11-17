//! T3 operator entrypoint tests consume the prebuilt binary, with isolated files.
use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

pub(super) fn token() -> String {
    let claims = json!({
        "exp": 4_000_000_000_u64,
        "https://api.openai.com/auth": {"chatgpt_account_id":"synthetic-account-1234"}, // lint-rules: allow-literal inert JWT claim key
    });
    format!("e30.{}.synthetic", URL_SAFE_NO_PAD.encode(claims.to_string()))
}

pub(super) fn config(directory: &Path, mode: &str, auth: &str) -> PathBuf {
    let template = include_str!("../../../../examples/bridge.example.toml")
        .replace("echo.example.invalid", "127.0.0.1")
        .replace("0.0.0.0:16900", "127.0.0.1:0") // lint-rules: allow-literal replace inert example listener with loopback
        .replace("credentials/live.json", "credential")
        .replace("mode = \"chatgpt\"", &format!("mode = \"{mode}\""));
    let path = directory.join("bridge.toml");
    std::fs::write(&path, format!("{template}\n{auth}")).unwrap();
    path
}

pub(super) fn command(directory: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_echo-bridge"));
    command.current_dir(directory).args(args).kill_on_drop(true);
    for name in [
        "ECHO_LAB_WYOMING_ADDR",
        "ECHO_LAB_LIVE_URL",
        "ECHO_LAB_TRACE_PATH",
        "ECHO_LAB_SESSION_IDLE_SECONDS",
    ] {
        command.env_remove(name);
    }
    command
}

pub(super) async fn invoke(directory: &Path, args: &[&str], code: i32, stdout: &str) {
    let output = tokio::time::timeout(OBSERVATION_TIMEOUT, command(directory, args).output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.status.code(), Some(code));
    assert!(output.stdout == stdout.as_bytes(), "operator output mismatch");
    let expected = match code {
        0 => "",
        2 => "echo-bridge: invalid input\n",
        _ => "echo-bridge: authorization\n",
    };
    assert!(output.stderr == expected.as_bytes(), "operator diagnostic mismatch");
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains(&token()),
        "credential disclosure"
    );
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_auth_import_status_and_no_implicit_discovery() {
    let directory = tempfile::tempdir().unwrap();
    config(directory.path(), "chatgpt", "");
    let original = serde_json::to_vec(&json!({
        "tokens": {"access_token":token(), "refresh_token":"synthetic-refresh", "account_id":"synthetic-account-1234"},
        "last_refresh":"2096-01-01T00:00:00Z",
    }))
    .unwrap();
    std::fs::write(directory.path().join("source"), &original).unwrap();
    let args = ["import-codex", "--config", "bridge.toml", "--path", "source"];
    invoke(directory.path(), &args, 0, "imported\n").await;
    let credential = std::fs::read(directory.path().join("credential")).unwrap();
    // Status must remain an offline metadata operation even when refresh is overdue.
    let mut record: Value = serde_json::from_slice(&credential).unwrap();
    record["last_refresh"] = 0.into();
    std::fs::write(
        directory.path().join("credential"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    invoke(
        directory.path(),
        &["status", "--config", "bridge.toml"],
        0,
        "mode: chatgpt\naccount: ****1234\nexpires: 4000000000\n",
    )
    .await;
    invoke(directory.path(), &args, 1, "").await;
    assert!(
        std::fs::read(directory.path().join("source")).unwrap() == original,
        "source changed"
    );
    std::fs::remove_file(directory.path().join("source")).unwrap();
    invoke(
        directory.path(),
        &["status", "--config", "bridge.toml"],
        0,
        "mode: chatgpt\naccount: ****1234\nexpires: 4000000000\n",
    )
    .await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_auth_api_status_needs_no_run_resources_or_credentials() {
    let directory = tempfile::tempdir().unwrap();
    config(directory.path(), "api_key", "");
    invoke(
        directory.path(),
        &["status", "--config", "bridge.toml"],
        0,
        "mode: api-key\naccount: n/a\nexpires: n/a\n",
    )
    .await;
    for args in [
        vec!["signin", "--config", "bridge.toml"],
        vec!["signin", "--config", "bridge.toml", "--browser"],
        vec!["import-codex", "--config", "bridge.toml", "--path", "missing"],
    ] {
        invoke(directory.path(), &args, 1, "").await;
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_auth_invalid_flags_and_missing_storage_fail_safely() {
    let directory = tempfile::tempdir().unwrap();
    config(directory.path(), "chatgpt", "");
    for args in [
        vec!["signin", "--config", "bridge.toml", "--browser", "--browser"],
        vec!["status", "--config", "bridge.toml", "--config", "bridge.toml"],
        vec!["status", "--config", "bridge.toml", "--trace", "unused"],
        vec!["import-codex", "--config", "bridge.toml"],
    ] {
        invoke(directory.path(), &args, 2, "").await;
    }
    invoke(directory.path(), &["status", "--config", "bridge.toml"], 1, "").await;
    std::fs::write(directory.path().join("source"), b"synthetic private invalid data").unwrap();
    invoke(
        directory.path(),
        &["import-codex", "--config", "bridge.toml", "--path", "source"],
        1,
        "",
    )
    .await;
    assert!(!directory.path().join("credential").exists());
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_auth_device_signin_commits_isolated_credential() {
    let directory = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = "http://127.0.0.1";
    let origin = format!("{base}:{}", listener.local_addr().unwrap().port());
    let auth = format!(
        "[chatgpt_auth]\nusercode_endpoint = '{origin}/user'\ndevice_token_endpoint = '{origin}/poll'\ntoken_endpoint = '{origin}/token'\nverification_url = '{origin}/verify'"
    );
    config(directory.path(), "chatgpt", &auth);
    let task = tokio::spawn(async move {
        let replies = [
            json!({"device_auth_id":"synthetic-device","user_code":"TEST-CODE","interval":1}),
            json!({"authorization_code":"synthetic-code","code_verifier":"synthetic-verifier"}),
            json!({"access_token":token(),"refresh_token":"synthetic-refresh"}),
        ];
        for (path, reply) in ["/user", "/poll", "/token"].into_iter().zip(replies) {
            let mut stream = listener.accept().await.unwrap().0;
            let request = request(&mut stream).await;
            assert!(
                request.starts_with(&format!("POST {path} HTTP/1.1\r\n")),
                "auth endpoint mismatch"
            );
            let body = reply.to_string();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    invoke(
        directory.path(),
        &["signin", "--config", "bridge.toml"],
        0,
        &format!("code: TEST-CODE\nurl: {origin}/verify\nsigned in\n"),
    )
    .await;
    task.await.unwrap();
    let bytes = std::fs::read(directory.path().join("credential")).unwrap();
    let stored: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        stored["access"].as_str() == Some(token().as_str()),
        "stored credential mismatch"
    );
    invoke(
        directory.path(),
        &["status", "--config", "bridge.toml"],
        0,
        "mode: chatgpt\naccount: ****1234\nexpires: 4000000000\n",
    )
    .await;
}

pub(super) async fn request(stream: &mut tokio::net::TcpStream) -> String {
    request_body(stream).await.0
}

pub(super) async fn request_body(stream: &mut tokio::net::TcpStream) -> (String, String) {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        bytes.push(stream.read_u8().await.unwrap());
        assert!(bytes.len() < 16_384);
    }
    let header = String::from_utf8(bytes).unwrap();
    let length: usize = header
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
        .map_or(0, |(_, value)| value.trim().parse().unwrap());
    assert!(length < 16_384);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.unwrap();
    (header, String::from_utf8(body).unwrap())
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_auth_configured_live_uses_selected_provider() {
    for mode in ["chatgpt", "api_key"] {
        let directory = tempfile::tempdir().unwrap();
        let origin = "http://127.0.0.1:1";
        let auth = format!("[chatgpt_auth]\ntoken_endpoint = '{origin}/token'");
        let path = config(directory.path(), mode, &auth);
        std::fs::create_dir(directory.path().join("credentials")).unwrap();
        std::fs::write(
            directory.path().join("credentials/screen-pairing.txt"),
            "synthetic-screen-pair\n",
        )
        .unwrap();
        prepare_configured_credential(directory.path(), mode).await;
        let fake = FakeLive::bind(FakeScript {
            events: vec![],
            response: FakeResponse::Http {
                status: 401,
                body: "synthetic private body".into(),
                location: None,
            },
        })
        .await
        .unwrap();
        let observer = fake.observer();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let content = std::fs::read_to_string(&path)
            .unwrap()
            .replace("wyoming_port = 16700", &format!("wyoming_port = {port}"));
        std::fs::write(&path, content).unwrap();
        let base = fake.base_url().trim_end_matches('/').to_owned();
        let mut cmd = command(
            directory.path(),
            &[
                "--config",
                "bridge.toml",
                "--trace",
                "trace",
                "--health-listen",
                "127.0.0.1:0",
                "--live-loopback",
                &base,
                "--model-loopback",
                &base,
            ],
        );
        let mut bridge = Process::spawn(&mut cmd);
        let mut stream = listener.accept().await.unwrap().0;
        bridge_short::register(&mut stream, &mut bridge).await;
        let stream = bridge_short::Satellite::new(stream);
        tokio::time::timeout(OBSERVATION_TIMEOUT, observer.wait_for_calls(1))
            .await
            .unwrap()
            .unwrap();
        super::bridge_wire_auth_failure::wait_event(&directory.path().join("trace"), "auth.relink_required").await;
        bridge.finish(0).await;
        assert_configured_headers(&observer, mode, directory.path());
        drop(stream);
        fake.shutdown().await;
    }
}

fn assert_configured_headers(observer: &echo_live::fake::FakeObserver, mode: &str, directory: &Path) {
    let calls = observer.calls().unwrap();
    assert_eq!(calls.len(), 1);
    let expected = if mode == "chatgpt" {
        token()
    } else {
        "synthetic-api-key".into()
    };
    assert!(
        calls[0]
            .request
            .header_matches("authorization", &format!("Bearer {expected}")),
        "credential header mismatch"
    );
    assert_eq!(calls[0].request.has_header("chatgpt-account-id"), mode == "chatgpt");
    if mode == "chatgpt" {
        assert!(
            calls[0]
                .request
                .header_matches("chatgpt-account-id", "synthetic-account-1234"),
            "account header mismatch"
        );
    }
    let path = directory.join("trace");
    let entries = super::trace(&path);
    assert_eq!(entries.last().unwrap()["reason"], "authorization");
    assert_eq!(entries.iter().filter(|e| e["event"] == "reply.started").count(), 0);
    let trace = std::fs::read_to_string(path).unwrap();
    assert!(
        !trace.contains(&expected) && !trace.contains("synthetic-account-1234"),
        "trace credential disclosure"
    );
}

pub(super) async fn prepare_configured_credential(directory: &Path, mode: &str) {
    if mode == "chatgpt" {
        let source = json!({"tokens":{"access_token":token(),"refresh_token":"synthetic-refresh",
                "account_id":"synthetic-account-1234"},"last_refresh":"2096-01-01T00:00:00Z"});
        std::fs::write(directory.join("source"), source.to_string()).unwrap();
        invoke(
            directory,
            &["import-codex", "--config", "bridge.toml", "--path", "source"],
            0,
            "imported\n",
        )
        .await;
    } else {
        std::fs::write(directory.join("credential"), "synthetic-api-key\r\n").unwrap();
    }
}

pub(super) fn fields(query: &str) -> std::collections::HashMap<String, String> {
    let mut url = reqwest::Url::parse("http://127.0.0.1/").unwrap();
    url.set_query(Some(query));
    url.query_pairs().into_owned().collect()
}

pub(super) async fn browser_exchange(listener: tokio::net::TcpListener, redirect: String) {
    let mut socket = listener.accept().await.unwrap().0;
    let (head, body) = request_body(&mut socket).await;
    assert!(head.starts_with("POST /token HTTP/1.1\r\n"), "token route mismatch");
    let form = fields(&body);
    assert_eq!(form.len(), 5);
    for (key, value) in [
        ("grant_type", "authorization_code"),
        ("code", "synthetic-code"),
        ("client_id", "fixture-client"),
        ("redirect_uri", redirect.as_str()),
    ] {
        assert!(
            form.get(key).is_some_and(|actual| actual == value),
            "exchange form mismatch"
        );
    }
    assert_eq!(form["code_verifier"].len(), 43);
    let body = json!({"access_token":token(),"refresh_token":"synthetic-browser-refresh"}).to_string();
    socket
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
}

pub(super) fn browser_query(url: &reqwest::Url) -> std::collections::HashMap<String, String> {
    let query = fields(url.query().unwrap());
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["code_challenge"].len(), 43);
    assert_eq!(query["state"].len(), 43);
    assert_eq!(query["client_id"], "fixture-client");
    assert_eq!(query["response_type"], "code");
    let callback = reqwest::Url::parse(&query["redirect_uri"]).unwrap();
    assert_eq!(callback.host_str(), Some("127.0.0.1"));
    assert_eq!(callback.path(), "/auth/callback");
    assert!(callback.port().is_some_and(|port| port != 0));
    query
}

pub(super) async fn browser_failure(process: &mut Process) {
    let status = tokio::time::timeout(OBSERVATION_TIMEOUT, process.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code(), Some(1));
    let error = process.stderr().await;
    assert!(error == "echo-bridge: authorization\n", "unsafe diagnostic");
    assert!(process.lines.next_line().await.unwrap().is_none());
}

pub(super) fn callback_query(callback: &mut reqwest::Url, state: &str, outcome: &str) {
    let (key, value) = if outcome == "denied" {
        ("error", "access_denied")
    } else {
        ("code", "synthetic-code")
    };
    callback.query_pairs_mut().extend_pairs([
        (key, value),
        ("state", if outcome == "wrong-state" { "wrong" } else { state }),
    ]);
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn mcp_link_epoch_without_guard_is_refused_before_configuration_or_dial() {
    let directory = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = "http://127.0.0.1";
    let origin = format!("{base}:{}", listener.local_addr().unwrap().port());
    let args = [
        "--config",
        "absent",
        "--trace",
        "absent-trace",
        "--live-loopback",
        &origin,
        "--model-loopback",
        &origin,
        "--sim-epoch",
        "1800000000",
    ];
    invoke(directory.path(), &args, 2, "").await;
    assert!(!directory.path().join("absent-trace").exists());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    assert!(tokio::time::timeout(Duration::ZERO, listener.accept()).await.is_err());
}
