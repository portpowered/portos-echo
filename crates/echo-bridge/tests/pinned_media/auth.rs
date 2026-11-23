//! T3: real pinned auth manager, inert private store and refusing local refresh.
use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use echo_codex::codex_supervisor::{HomeMode, Supervisor, SupervisorConfig};
use std::path::PathBuf;

#[tokio::test]
async fn t3_pinned_codex_invalid_and_expired_auth_use_only_local_refresh() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(exercise()))
        .await
        .unwrap();
}

// Seed the selected home with an expired fixture. No nominated API key means
// authenticate returns before issuing account RPCs, so both refresh requests
// originate during pinned-server startup.
#[tokio::test]
async fn t3_pinned_startup_refreshes_before_bridge_account_authentication() {
    tokio::time::timeout(Duration::from_secs(25), rejection(true, LiveMode::ApiKey))
        .await
        .unwrap();
}

// Codex performs two startup refreshes (first measured at rust-v0.160.0); a release that changes the count fails here.
const EXPIRED_REFRESHES: u64 = 2;

async fn exercise() {
    for expired in [false, true] {
        rejection(expired, LiveMode::Chatgpt).await;
    }
}

async fn rejection(expired: bool, mode: LiveMode) {
    let directory = tempfile::tempdir().unwrap();
    let config = pinned_config(directory.path());
    let (home, owner) = reserve_home(&config, mode);
    drop(owner);
    let auth = synthetic(expired);
    std::fs::write(home.join("auth.json"), &auth).unwrap();
    let (mut provider, mut input, mut output, origin) = provider().await;
    let routes = LocalRoutes::new(&origin, &origin).unwrap();
    let mut admitted = false;
    let result = with_host_options(
        HostOptions {
            config: &config,
            mode,
            api_key: None,
            routes: Some(&routes),
            deadline: Instant::now() + Duration::from_secs(10),
        },
        std::future::pending(),
        async |_| {
            admitted = true;
            Ok(())
        },
    )
    .await;
    assert_eq!(result, Err(echo_bridge::Error::Authorization));
    assert!(!admitted);
    input.write_all(b"{\"snapshot\":true}\n").await.unwrap();
    let observed: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(observed["calls"], json!([]));
    assert_eq!(observed["external"], 0);
    assert_eq!(observed["sidebands"], 0);
    assert_eq!(
        observed["refreshes"].as_u64().unwrap_or(0),
        if expired { EXPIRED_REFRESHES } else { 0 }
    );
    drop(input);
    assert!(provider.wait().await.unwrap().success());
    assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth);
    drop(reserve_home(&config, mode).1);
}

fn reserve_home(config: &CodexConfig, mode: LiveMode) -> (PathBuf, Supervisor) {
    let base = config.home.clone().unwrap();
    let mut supervisor = SupervisorConfig::new(config.binary.clone().unwrap(), base.clone());
    let (home, home_mode) = match mode {
        LiveMode::Chatgpt => (base, HomeMode::Chatgpt),
        LiveMode::ApiKey => (base.with_file_name("home-api-key"), HomeMode::ApiKey),
    };
    supervisor.home_mode = home_mode;
    (home, Supervisor::new(supervisor).unwrap())
}

fn synthetic(expired: bool) -> Vec<u8> {
    if !expired {
        return b"malformed synthetic store".to_vec();
    }
    let claims = json!({"exp":1,"email":"local@example.invalid",
        "https://api.openai.com/auth":{"chatgpt_account_id":"synthetic-local","chatgpt_plan_type":"plus"}}); // lint-rules: allow-literal inert synthetic JWT claim key
    let token = format!("e30.{}.synthetic", URL_SAFE_NO_PAD.encode(claims.to_string()));
    serde_json::to_vec(&json!({"auth_mode":"chatgpt","OPENAI_API_KEY":null,
        "tokens":{"id_token":token,"access_token":token,"refresh_token":"synthetic-refresh",
            "account_id":"synthetic-local"},"last_refresh":"2000-01-01T00:00:00Z"}))
    .unwrap()
}

async fn provider() -> (
    tokio::process::Child,
    tokio::process::ChildStdin,
    tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    String,
) {
    let mut provider = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pinned_media/provider.py"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let input = provider.stdin.take().unwrap();
    let mut output = BufReader::new(provider.stdout.take().unwrap()).lines();
    let ready: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    let origin = ready["origin"].as_str().unwrap();
    (provider, input, output, origin.into())
}

/// Delivered artifact: auth rejection precedes Wyoming and preserves the store.
#[tokio::test]
async fn t3_pinned_expired_executable_relinks_before_wyoming_admission() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(executable_rejection(true)))
        .await
        .unwrap();
}

#[tokio::test]
async fn t3_pinned_invalid_executable_relinks_before_wyoming_admission() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(executable_rejection(false)))
        .await
        .unwrap();
}

async fn executable_rejection(expired: bool) {
    let directory = tempfile::tempdir().unwrap();

    let root = directory.path();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = super::voice::configure(root, listener.local_addr().unwrap().port());
    let home = config.home.unwrap();
    let auth = prepare_auth(root, &home, expired);
    let (provider, input, output, origin) = provider().await;
    let mut child = super::voice::launch(root, &origin);
    wait_relink(root).await;
    let mut screen = super::voice::lifecycle::screen(root).await;
    let denied = super::voice::lifecycle::next_frame(&mut screen).await.unwrap();
    assert_eq!(denied["seq"], 1);
    assert_eq!(
        denied["state"],
        json!({"state":"error", "relink":true,
        "message":"Run echo-bridge signin with your config, then restart the bridge."})
    );
    assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(listener.poll_accept(cx).is_pending())).await);
    drop(child.stdin.take());
    rejected_child(child, &mut screen).await;
    assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(listener.poll_accept(cx).is_pending())).await);
    executable_trace(root);
    provider_closed(provider, input, output, expired).await;
    assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth);
    drop(Supervisor::new(SupervisorConfig::new(binary(), home)).unwrap());
}

fn executable_trace(root: &Path) {
    let rows: Vec<Value> = std::fs::read_to_string(root.join("trace"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.iter().filter(|r| r["event"] == "auth.relink_required").count(), 1);
    for event in [
        "bridge.ready",
        "run.started",
        "live.connected",
        "live.closed",
        "mic.completed",
        "reply.played",
    ] {
        assert_eq!(rows.iter().filter(|r| r["event"] == event).count(), 0, "{event}");
    }
}

fn prepare_auth(root: &Path, home: &Path, expired: bool) -> Vec<u8> {
    let path = root.join("bridge.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("mode = \"api_key\"", "mode = \"chatgpt\"");
    std::fs::write(path, text).unwrap();
    drop(Supervisor::new(SupervisorConfig::new(binary(), home.into())).unwrap());
    let auth = synthetic(expired);
    std::fs::write(home.join("auth.json"), &auth).unwrap();
    auth
}

async fn rejected_child(child: tokio::process::Child, screen: &mut tokio::io::Lines<BufReader<tokio::net::TcpStream>>) {
    let result = child.wait_with_output().await.unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(result.stdout, b"");
    assert_eq!(result.stderr, b"echo-bridge: authorization\n");
    assert_eq!(super::voice::lifecycle::next_frame(screen).await, None);
}

async fn wait_relink(root: &Path) {
    loop {
        let trace = std::fs::read_to_string(root.join("trace")).unwrap_or_default();
        if trace
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .any(|row| row["event"] == "auth.relink_required")
        {
            return;
        }
        tokio::task::yield_now().await;
    }
}

async fn provider_closed(
    mut provider: tokio::process::Child,
    mut input: tokio::process::ChildStdin,
    mut output: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    expired: bool,
) {
    input.write_all(b"{\"snapshot\":true}\n").await.unwrap();
    let observed: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    let mut expected = json!({"calls":[], "sidebands":0, "external":0});
    if expired {
        expected["refreshes"] = EXPIRED_REFRESHES.into();
    }
    assert_eq!(observed, expected);
    drop(input);
    assert!(provider.wait().await.unwrap().success());
}
