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
        // These fixtures own reply/control behavior; wake tests exercise the chime.
        .replace("wake_earcon = true", "wake_earcon = false")
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
        let trace = directory.path().join("trace");
        super::bridge_wire_auth_failure::wait_event(&trace, "auth.relink_required", &mut bridge).await;
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
    assert_authorization_relink(&entries);
    assert_eq!(entries.iter().filter(|e| e["event"] == "reply.started").count(), 0);
    let trace = std::fs::read_to_string(path).unwrap();
    assert!(
        !trace.contains(&expected) && !trace.contains("synthetic-account-1234"),
        "trace credential disclosure"
    );
}

fn assert_authorization_relink(entries: &[Value]) {
    let relinks: Vec<_> = entries
        .iter()
        .filter(|entry| entry["event"] == "auth.relink_required")
        .collect();
    assert_eq!(relinks.len(), 1);
    assert_eq!(relinks[0]["reason"], "authorization");
}

#[test]
fn bridge_wire_auth_relink_oracle_trailing_probe() {
    assert_authorization_relink(&[
        json!({"event":"auth.relink_required", "reason":"authorization"}),
        json!({"event":"wyoming.probe_ok"}),
    ]);
}

#[test]
fn bridge_wire_auth_relink_oracle_last_relink() {
    assert_authorization_relink(&[json!({"event":"auth.relink_required", "reason":"authorization"})]);
}

#[test]
fn bridge_wire_auth_relink_oracle_rejects_invalid_traces() {
    let relink = json!({"event":"auth.relink_required", "reason":"authorization"});
    for entries in [
        vec![],
        vec![json!({"event":"wyoming.probe_ok"})],
        vec![json!({"event":"unrelated", "reason":"authorization"})],
        vec![relink.clone(), relink],
        vec![json!({"event":"auth.relink_required", "reason":"unavailable"})],
        vec![json!({"event":"auth.relink_required"})],
    ] {
        assert!(std::panic::catch_unwind(|| assert_authorization_relink(&entries)).is_err());
    }
}

pub(super) async fn prepare_configured_credential(directory: &Path, mode: &str) {
    if mode == "chatgpt" {
        let source = json!({"tokens":{"access_token":token(),"refresh_token":"synthetic-refresh",
                "account_id":"synthetic-account-1234"},"last_refresh":"2096-01-01T00:00:00Z"});
        std::fs::write(directory.join("source"), source.to_string()).unwrap();
        // This helper protects the not-yet-migrated voice consumer, not the canonical CLI.
        legacy_providers(directory)
            .execute(
                echo_signin::OperatorCommand::ImportCodex {
                    path: directory.join("source"),
                },
                &mut Vec::new(),
            )
            .await
            .unwrap();
    } else {
        std::fs::write(directory.join("credential"), "synthetic-api-key\r\n").unwrap();
    }
}

pub(super) fn fields(query: &str) -> std::collections::HashMap<String, String> {
    let mut url = reqwest::Url::parse("http://127.0.0.1/").unwrap();
    url.set_query(Some(query));
    url.query_pairs().into_owned().collect()
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

// Temporary fixture boundary for the retained legacy voice-runtime tests.
// Canonical operator tests use the app-server process in codex_operator.
pub(super) fn legacy_providers(directory: &Path) -> echo_signin::Providers {
    let mut config = echo_config::BridgeConfig::load(directory.join("bridge.toml")).unwrap();
    config.live.credential_file = directory.join(config.live.credential_file);
    echo_signin::resolve_mode(config.live, config.chatgpt_auth).unwrap()
}
