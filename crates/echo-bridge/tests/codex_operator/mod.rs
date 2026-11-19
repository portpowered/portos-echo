//! T3: delivered CLI, controlled stdio account process and paired loopback screen.
use super::*;
mod configured;
mod device;
mod fixture;
mod host;
mod recovery;
mod tools;
mod voice_entry;
use fixture::Fixture;

impl Fixture {
    pub async fn invoke(&self, args: &[&str], code: i32, stdout: &str, stderr: &str) {
        let output = tokio::time::timeout(
            OBSERVATION_TIMEOUT,
            bridge_wire_auth::command(self.directory.path(), args)
                .env("OPENAI_API_KEY", "synthetic-inherited-key")
                .env("OPENAI_ACCESS_TOKEN", "synthetic-inherited-token")
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(output.status.code(), Some(code));
        assert_eq!(String::from_utf8(output.stdout).unwrap(), stdout);
        assert_eq!(String::from_utf8(output.stderr).unwrap(), stderr);
    }
}

fn presentation_owned(path: &Path) {
    assert!(
        trace(path)
            .iter()
            .all(|row| row["event"] != "health.released" && row["event"] != "screen.released")
    );
}

fn presentation_released(path: &Path, mode: &str, screen: bool) {
    let rows = trace(path);
    let receipts: Vec<_> = rows
        .iter()
        .filter(|row| matches!(row["event"].as_str(), Some("health.released" | "screen.released")))
        .collect();
    let expected = if screen {
        vec!["health.released", "screen.released"]
    } else {
        vec!["health.released"]
    };
    assert_eq!(
        receipts
            .iter()
            .map(|row| row["event"].as_str().unwrap())
            .collect::<Vec<_>>(),
        expected
    );
    for receipt in receipts {
        let screen = receipt["event"] == "screen.released";
        assert_eq!(receipt.as_object().unwrap().len(), if screen { 4 } else { 3 });
        if screen {
            assert_eq!(receipt["dropped_clients"], 0);
        }
        assert_eq!(receipt["provider_auth_mode"], mode);
        assert!(receipt["elapsed_ms"].is_u64());
    }
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge and local Python"]
async fn codex_operator_signin_screen_matching_completion_and_reopened_status() {
    for browser in [false, true] {
        let fixture = Fixture::new("chatgpt");
        let args = if browser {
            vec!["signin", "--config", "bridge.toml", "--browser"]
        } else {
            vec!["signin", "--config", "bridge.toml"]
        };
        let mut process = Process::spawn(&mut bridge_wire_auth::command(fixture.directory.path(), &args));
        let prompt = if browser {
            assert_eq!(process.line().await, "url: http://127.0.0.1:12345/authorize");
            "url: http://127.0.0.1:12345/authorize\n"
        } else {
            assert_eq!(process.line().await, "code: TEST-CODE");
            assert_eq!(process.line().await, "url: http://127.0.0.1:12345/verify");
            "code: TEST-CODE\nurl: http://127.0.0.1:12345/verify\n"
        };
        let mut control = fixture.control().await;
        let mut screen = bridge_wire_screen::stream(&fixture.screen.to_string()).await;
        let state = bridge_wire_screen::state(&mut screen).await;
        assert_eq!(state, json!({"state":"ready", "message":prompt}));
        // The peer has already sent a successful foreign completion. It cannot finish login.
        assert!(process.child.try_wait().unwrap().is_none());
        assert!(!fixture.home.join("auth.json").exists());
        control.write_all(b"success").await.unwrap();
        assert_eq!(process.line().await, "signed in");
        process.collect(0).await;
        assert_eq!(process.stderr().await, "");
        assert!(process.lines.next_line().await.unwrap().is_none());
        voice_entry::screen_ended(&mut screen).await;
        let before = std::fs::read(fixture.home.join("auth.json")).unwrap();
        assert_eq!(before, b"{\"synthetic\":\"local-only\"}\n");
        fixture
            .invoke(
                &["status", "--config", "bridge.toml"],
                0,
                "mode: chatgpt\naccount: ****\nexpires: n/a\n",
                "",
            )
            .await;
        assert_eq!(std::fs::read(fixture.home.join("auth.json")).unwrap(), before);
        assert!(!fixture.directory.path().join("credential").exists());
        // Collection destroys this executable's exclusively owned listeners.
        assert_eq!(process.child.id(), None);
        drop(fixture.owner());
    }
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge and local Python"]
async fn codex_operator_failed_completion_and_eof_preserve_auth_and_release_resources() {
    for outcome in ["failure", "eof"] {
        let fixture = Fixture::new("chatgpt");
        std::fs::write(fixture.home.join("auth.json"), b"synthetic prior state").unwrap();
        let mut process = Process::spawn(&mut bridge_wire_auth::command(
            fixture.directory.path(),
            &["signin", "--config", "bridge.toml"],
        ));
        assert_eq!(process.line().await, "code: TEST-CODE");
        assert_eq!(process.line().await, "url: http://127.0.0.1:12345/verify");
        let mut control = fixture.control().await;
        let mut screen = bridge_wire_screen::stream(&fixture.screen.to_string()).await;
        assert_eq!(bridge_wire_screen::state(&mut screen).await["state"], "ready");
        control.write_all(outcome.as_bytes()).await.unwrap();
        assert_eq!(
            bridge_wire_screen::state(&mut screen).await,
            json!({"state":"error", "relink":true,
            "message":"Run echo-bridge signin with your config, then restart the bridge."})
        );
        process.collect(1).await;
        assert_eq!(process.stderr().await, "echo-bridge: authorization\n");
        assert!(process.lines.next_line().await.unwrap().is_none());
        assert_eq!(
            std::fs::read(fixture.home.join("auth.json")).unwrap(),
            b"synthetic prior state"
        );
        drop(fixture.owner());
        assert_eq!(process.child.id(), None);
        voice_entry::screen_ended(&mut screen).await;
    }
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge and local Python"]
async fn codex_operator_import_missing_binary_home_exclusion_and_kill_are_safe() {
    let fixture = Fixture::new("chatgpt");
    let path = fixture.directory.path();
    std::fs::write(path.join("source"), "synthetic source bytes").unwrap();
    fixture
        .invoke(
            &["import-codex", "--config", "bridge.toml", "--path", "source"],
            2,
            "",
            "echo-bridge: use signin with the configured Codex home\n",
        )
        .await;
    assert_eq!(std::fs::read(path.join("source")).unwrap(), b"synthetic source bytes");
    assert!(!path.join("credential").exists());
    assert!(!fixture.home.join("peer-pid").exists());
    let owner = fixture.owner();
    fixture
        .invoke(
            &["status", "--config", "bridge.toml"],
            2,
            "",
            "echo-bridge: codex.home: private ownership unavailable\n",
        )
        .await;
    fixture
        .invoke(&["kill", "--config", "bridge.toml"], 0, "kill-switch: on\n", "")
        .await;
    fixture
        .invoke(
            &["kill", "--config", "bridge.toml", "--clear"],
            0,
            "kill-switch: off\n",
            "",
        )
        .await;
    drop(owner);
    let config = path.join("bridge.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    let missing = path.join("absent-binary");
    std::fs::write(
        &config,
        text.replace(&fixture.binary.display().to_string(), &missing.display().to_string()),
    )
    .unwrap();
    fixture
        .invoke(
            &["status", "--config", "bridge.toml"],
            2,
            "",
            "echo-bridge: codex.binary: unavailable\n",
        )
        .await;
    assert!(!fixture.home.join("peer-pid").exists());
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge and local Python"]
async fn codex_operator_api_key_status_and_invalid_argv_do_not_acquire_legacy_credentials() {
    let fixture = Fixture::new("api_key");
    fixture
        .invoke(
            &["status", "--config", "bridge.toml"],
            0,
            "mode: api-key\naccount: n/a\nexpires: n/a\n",
            "",
        )
        .await;
    fixture
        .invoke(
            &["signin", "--config", "bridge.toml"],
            1,
            "",
            "echo-bridge: authorization\n",
        )
        .await;
    for args in [
        vec!["signin", "--config", "absent", "--browser", "--browser"],
        vec!["status", "--config", "absent", "--trace", "unused"],
        vec!["import-codex", "--config", "absent"],
    ] {
        fixture.invoke(&args, 2, "", "echo-bridge: invalid input\n").await;
    }
    assert!(!fixture.directory.path().join("credential").exists());
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge and the offline helper"]
async fn codex_operator_required_home_and_offline_cache_miss_fail_before_spawn() {
    let fixture = Fixture::new("chatgpt");
    let path = fixture.directory.path().join("bridge.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.split("[codex]").next().unwrap()).unwrap();
    fixture
        .invoke(
            &["status", "--config", "bridge.toml"],
            2,
            "",
            "echo-bridge: codex.home: required for Codex runtime\n",
        )
        .await;
    let binary_line = format!("binary = '{}'\n", fixture.binary.display());
    std::fs::write(&path, text.replace(&binary_line, "")).unwrap();
    let output = tokio::time::timeout(
        OBSERVATION_TIMEOUT,
        bridge_wire_auth::command(fixture.directory.path(), &["status", "--config", "bridge.toml"])
            .env("ECHO_CODEX_CACHE_DIR", fixture.directory.path().join("empty-cache"))
            .env("ECHO_CODEX_OFFLINE", "0")
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(output.stdout, b"");
    assert_eq!(output.stderr, b"echo-bridge: codex.binary: unavailable\n");
    assert!(!fixture.home.join("peer-pid").exists());
    assert!(!fixture.directory.path().join("credential").exists());
    drop(fixture.owner());
}
