//! T3: real pinned auth manager, inert private store and refusing local refresh.
use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use echo_codex::codex_supervisor::{Supervisor, SupervisorConfig};

#[tokio::test]
async fn t3_pinned_codex_invalid_and_expired_auth_use_only_local_refresh() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(exercise()))
        .await
        .unwrap();
}

async fn exercise() {
    for expired in [false, true] {
        rejection(expired).await;
    }
}

async fn rejection(expired: bool) {
    let directory = tempfile::tempdir().unwrap();
    private_parent(directory.path());
    let home = directory.path().join("home");
    let config = CodexConfig {
        binary: Some(binary()),
        home: Some(home.clone()),
        ..CodexConfig::default()
    };
    drop(Supervisor::new(SupervisorConfig::new(binary(), home.clone())).unwrap());
    let auth = synthetic(expired);
    std::fs::write(home.join("auth.json"), &auth).unwrap();
    let (mut provider, mut input, mut output, origin) = provider().await;
    let routes = LocalRoutes::new(&origin, &origin).unwrap();
    let mut admitted = false;
    let result = with_host_options(
        HostOptions {
            config: &config,
            mode: LiveMode::Chatgpt,
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
    assert_eq!(observed["refreshes"].as_u64().unwrap_or(0), if expired { 2 } else { 0 });
    drop(input);
    assert!(provider.wait().await.unwrap().success());
    assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth);
    drop(Supervisor::new(SupervisorConfig::new(binary(), home)).unwrap());
}

fn synthetic(expired: bool) -> Vec<u8> {
    if !expired {
        return b"malformed synthetic store".to_vec();
    }
    let claims = json!({"exp":1,"email":"local@example.invalid",
        "https://api.openai.com/auth":{"chatgpt_account_id":"synthetic-local","chatgpt_plan_type":"plus"}});
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
    let mut provider = Command::new(if cfg!(windows) { "python" } else { "python3" })
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
