//! T3: restart the delivered bridge against the file-backed fake account process.
use super::*;
use echo_codex::codex_supervisor::{HomeMode, Supervisor, SupervisorConfig};
use std::{fs, os::unix::fs::PermissionsExt};

const KEY: &str = "synthetic-nominated-key";
const KEY_FILE: &[u8] = b"synthetic-nominated-key\r\n";
const CHATGPT_AUTH: &[u8] = b"{\"auth_mode\":\"chatgpt\", \"tokens\":{\"access_token\":\"synthetic-chat-session\"}}\n";

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge and local Python"]
async fn auth_mode_isolation_switches_homes_and_preserves_chatgpt_account() {
    let (fixture, listener) = installation().await;
    let root = fixture.directory.path();
    let api_home = root.join("home-api-key");
    let key_file = root.join("credential");
    fs::write(&key_file, KEY_FILE).unwrap();
    fs::set_permissions(&key_file, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!api_home.exists());

    set_mode(&fixture, "chatgpt", "api_key");
    start_and_stop(&fixture, &listener, &api_home, "api_key").await;
    assert_state(&api_home, ["API model cache", "API logs fixture", "API work state"]);
    assert_base_unchanged(&fixture);
    assert!(!fixture.home.join("launches").exists());
    let methods = fs::read_to_string(api_home.join("methods")).unwrap();
    assert_eq!(
        methods
            .lines()
            .filter(|method| method.starts_with("account/"))
            .collect::<Vec<_>>(),
        ["account/login/start", "account/read"]
    );
    let stored: Value = serde_json::from_slice(&fs::read(api_home.join("auth.json")).unwrap()).unwrap();
    assert_eq!(stored["auth_mode"], "api_key");
    assert!(
        stored["OPENAI_API_KEY"] == KEY,
        "fake API login did not persist the nominated value"
    );

    set_mode(&fixture, "api_key", "chatgpt");
    start_and_stop(&fixture, &listener, &fixture.home, "chatgpt").await;
    assert_chatgpt_state(&fixture);
    assert_eq!(read(&api_home, "models_cache.json"), "API model cache");
    assert_eq!(read(&api_home, "logs_1.sqlite"), "API logs fixture");
    assert_eq!(read(&api_home, "work/work-fixture"), "API work state");
    assert!(!api_home.join("auth.json").exists());
    assert!(fs::read(key_file).unwrap() == KEY_FILE, "nominated key source changed");
    assert_key_absent(&fixture.home);
    assert_key_absent(&api_home);
    eprintln!("auth mode switch: ready=1+1, relink=0, ChatGPT auth preserved, API auth copy removed");
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge and local Python"]
async fn auth_mode_isolation_chatgpt_accepts_absent_api_home_and_absent_api_auth() {
    let (fixture, listener) = installation().await;
    start_and_stop(&fixture, &listener, &fixture.home, "chatgpt").await;
    let api_home = fixture.directory.path().join("home-api-key");
    assert!(!api_home.exists());
    drop(api_owner(&fixture));
    fs::write(api_home.join("keep"), "owned non-auth state").unwrap();
    fs::remove_file(fixture.home.join("launches")).unwrap();
    start_and_stop(&fixture, &listener, &fixture.home, "chatgpt").await;
    assert_base_unchanged(&fixture);
    assert!(!api_home.join("auth.json").exists());
    assert_eq!(read(&api_home, "keep"), "owned non-auth state");
    assert!(!fixture.directory.path().join("credential").exists());
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge and local Python"]
async fn auth_mode_isolation_refuses_unsafe_cleanup_then_recovers() {
    let (fixture, listener) = installation().await;
    drop(api_owner(&fixture));
    let api_auth = fixture.directory.path().join("home-api-key/auth.json");
    let key_file = fixture.directory.path().join("credential");
    fs::write(&key_file, KEY_FILE).unwrap();
    fs::create_dir(&api_auth).unwrap();
    fs::write(api_auth.join("sentinel"), "keep").unwrap();

    let mut process = Process::spawn(&mut command(&fixture));
    process.collect(2).await;
    process.assert_collected();
    assert_eq!(
        process.stderr().await,
        "echo-bridge: codex.home: private ownership unavailable\n"
    );
    assert!(process.lines.next_line().await.unwrap().is_none());
    assert!(!fixture.home.join("launches").exists());
    assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(listener.poll_accept(cx).is_pending())).await);
    assert_eq!(read(&api_auth, "sentinel"), "keep");
    let rows = trace_if_present(&fixture.directory.path().join("trace"));
    assert_eq!(count(&rows, "bridge.ready"), 0);
    assert_eq!(count(&rows, "auth.relink_required"), 0);
    assert_base_unchanged(&fixture);

    fs::remove_file(api_auth.join("sentinel")).unwrap();
    fs::remove_dir(api_auth).unwrap();
    start_and_stop(&fixture, &listener, &fixture.home, "chatgpt").await;
    assert_chatgpt_state(&fixture);
    assert!(fs::read(key_file).unwrap() == KEY_FILE, "nominated key source changed");
    eprintln!("unsafe cleanup: exit=2, child starts=0, safe retry ready=1");
}

async fn installation() -> (Fixture, TcpListener) {
    let fixture = Fixture::new("chatgpt");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    configure(&fixture, listener.local_addr().unwrap().port());
    let root = fixture.directory.path();
    let peer = root.join("auth-mode-peer");
    fs::write(&peer, concat!("#!/usr/bin/env python3\n", include_str!("../peer.py"))).unwrap();
    fs::set_permissions(&peer, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(root.join("auth-mode-fixture"), "read-stored-account").unwrap();
    fs::copy(fixture.home.join("lifetime-port"), root.join("lifetime-port")).unwrap();
    let path = root.join("bridge.toml");
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace(
            &format!("binary = '{}'", fixture.binary.display()),
            &format!("binary = '{}'", peer.display()),
        )
        .replace("session_max_seconds = 600", "session_max_seconds = 60");
    fs::write(path, text).unwrap();
    fs::write(fixture.home.join("auth.json"), CHATGPT_AUTH).unwrap();
    fs::write(fixture.home.join("models_cache.json"), "ChatGPT model cache").unwrap();
    fs::write(fixture.home.join("logs_1.sqlite"), "ChatGPT logs fixture").unwrap();
    fs::write(fixture.home.join("work/work-fixture"), "ChatGPT work state").unwrap();
    (fixture, listener)
}

async fn start_and_stop(fixture: &Fixture, listener: &TcpListener, home: &Path, mode: &str) {
    let trace_path = fixture.directory.path().join("trace");
    if trace_path.exists() {
        fs::remove_file(&trace_path).unwrap();
    }
    let mut process = Process::spawn(&mut command(fixture));
    let mut satellite = tokio::time::timeout(OBSERVATION_TIMEOUT, listener.accept())
        .await
        .unwrap()
        .unwrap()
        .0;
    assert_eq!(
        fixture::registration(&mut satellite).await,
        json!({"hassmic_failsafe_seconds":65})
    );
    assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "describe");
    bridge_short::send(&mut satellite, "info", json!({"satellite":{"name":"synthetic"}})).await;
    assert_eq!(process.line().await, r#"{"event":"bridge.ready"}"#);
    let child = fixture.child_lifetime().await;
    assert_eq!(
        child.try_read(&mut [0]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    process.finish(0).await;
    process.assert_collected();
    assert_eq!(process.stderr().await, "");
    assert!(process.lines.next_line().await.unwrap().is_none());
    tokio::time::timeout(OBSERVATION_TIMEOUT, Fixture::child_closed(child))
        .await
        .unwrap();
    tokio::time::timeout(OBSERVATION_TIMEOUT, satellite.read_to_end(&mut Vec::new()))
        .await
        .unwrap()
        .unwrap();
    if mode == "api_key" {
        drop(api_owner(fixture));
    } else {
        drop(fixture.owner());
    }
    assert_eq!(read(home, "launches"), "started\n");
    let rows = trace(&trace_path);
    assert_eq!(count(&rows, "bridge.ready"), 1);
    assert_eq!(count(&rows, "auth.relink_required"), 0);
    assert!(!strings_contain_key(&fs::read(&trace_path).unwrap()));
    presentation_released(&trace_path, mode, true);
}

fn api_owner(fixture: &Fixture) -> Supervisor {
    let mut config = SupervisorConfig::new(fixture.binary.clone(), fixture.home.clone());
    config.home_mode = HomeMode::ApiKey;
    Supervisor::new(config).unwrap()
}

fn set_mode(fixture: &Fixture, previous: &str, next: &str) {
    let path = fixture.directory.path().join("bridge.toml");
    let text = fs::read_to_string(&path).unwrap();
    let before = format!("mode = \"{previous}\"");
    assert_eq!(text.matches(&before).count(), 1);
    fs::write(path, text.replace(&before, &format!("mode = \"{next}\""))).unwrap();
}

fn assert_chatgpt_state(fixture: &Fixture) {
    assert_base_unchanged(fixture);
    assert_state(
        &fixture.home,
        ["ChatGPT model cache", "ChatGPT logs fixture", "ChatGPT work state"],
    );
    let methods = read(&fixture.home, "methods");
    assert_eq!(
        methods
            .lines()
            .filter(|method| method.starts_with("account/"))
            .collect::<Vec<_>>(),
        ["account/read"]
    );
}

fn assert_base_unchanged(fixture: &Fixture) {
    assert!(
        fs::read(fixture.home.join("auth.json")).unwrap() == CHATGPT_AUTH,
        "original ChatGPT auth changed"
    );
    assert_eq!(read(&fixture.home, "models_cache.json"), "ChatGPT model cache");
    assert_eq!(read(&fixture.home, "logs_1.sqlite"), "ChatGPT logs fixture");
    assert_eq!(read(&fixture.home, "work/work-fixture"), "ChatGPT work state");
}

fn assert_state(home: &Path, [cache, logs, work]: [&str; 3]) {
    let actual: Value = serde_json::from_str(&read(home, "home-state.json")).unwrap();
    assert_eq!(
        actual,
        json!({
            "codex_home": home, "home": home, "cwd": home.join("work"),
            "cache": cache, "logs": logs, "work": work,
        })
    );
}

fn read(home: &Path, name: &str) -> String {
    fs::read_to_string(home.join(name)).unwrap()
}

fn assert_key_absent(home: &Path) {
    for entry in fs::read_dir(home).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            assert_key_absent(&entry.path());
        } else {
            assert!(
                !strings_contain_key(&fs::read(entry.path()).unwrap()),
                "API key survived in app-server state"
            );
        }
    }
}

fn strings_contain_key(bytes: &[u8]) -> bool {
    bytes.windows(KEY.len()).any(|window| window == KEY.as_bytes())
}

fn count(rows: &[Value], event: &str) -> usize {
    rows.iter().filter(|row| row["event"] == event).count()
}
