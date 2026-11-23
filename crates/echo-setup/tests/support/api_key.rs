//! T3 API-key install: key bytes cross only adb stdin; `ChatGPT` sign-in is skipped.
use super::*;

const KEY: &str = "dummy-local-only";

fn api_key_fixture(mode: &str) -> Result<(Fixture, PathBuf)> {
    let fixture = Fixture::new()?;
    let base = include_str!("../../../../examples/bridge.echo.toml");
    fs::write(
        fixture.0.join("setup-config"),
        base.replace("mode = \"chatgpt\"", &format!("mode = \"{mode}\"")),
    )?;
    fs::write(fixture.0.join("link-state"), "linked")?;
    fs::write(
        fixture.0.join("api-key-status-reply"),
        "echo-install: api-key-status completed\r\n",
    )?;
    let key = fixture.0.join("key ü ' ; $.txt");
    fs::write(&key, format!("{KEY}\r\n"))?;
    Ok((fixture, key))
}

fn assert_key_absent(fixture: &Fixture, output: &Output) {
    let text = format!(
        "{}{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string(fixture.0.join("ledger")).unwrap_or_default()
    );
    assert!(!text.contains(KEY), "API key reached argv or output");
}

#[test]
fn t3_api_key_install_streams_key_skips_chatgpt_and_reports_mode() -> Result {
    let (fixture, key) = api_key_fixture("api_key")?;
    let output = fixture
        .install()
        .arg("--from-dir")
        .arg(fixture.bundle()?)
        .arg("--openai-api-key-file")
        .arg(&key)
        .output()?;
    assert_key_absent(&fixture, &output);
    let result = private_output(&output)?;
    assert_eq!(output.status.code(), Some(0), "{result}");
    assert_eq!(
        result,
        serde_json::json!({"type":"result", "command":"install", "ok":true, "version":"0.2.0", "setup":"complete"})
    );
    assert_eq!(fs::read(fixture.0.join("api-key-input"))?, KEY.as_bytes());
    let calls = fixture.ledger()?;
    let operations: Vec<&String> = calls.iter().filter_map(|args| args.last()).collect();
    let position = |needle: &str| operations.iter().position(|call| call.contains(needle));
    let streamed = position("umask 077 && cat > '/data/local/tmp/echo-setup-").ok_or("key not streamed")?;
    let published = position("--device api-key --source '").ok_or("key not published")?;
    let started = position("--device start").ok_or("bridge not started")?;
    assert!(streamed < published && published < started);
    assert!(operations[published].ends_with("/api-key' --file '/data/local/echo-bridge/private/live.json'"));
    for verb in ["--device ensure-signin", "--device signin", "--device link"] {
        assert_eq!(operations.iter().filter(|call| call.ends_with(verb)).count(), 0);
    }
    assert_eq!(
        operations
            .iter()
            .filter(|call| call.ends_with("--device api-key-status"))
            .count(),
        1
    );
    let status = fixture
        .command()
        .args(["status", "--json", "--adb", env!("CARGO_BIN_EXE_echo-setup-test-adb")])
        .output()?;
    assert_eq!(status.status.code(), Some(0));
    assert_eq!(private_output(&status)?["auth_mode"], "api_key");
    let doctor = fixture
        .command()
        .args(["doctor", "--json", "--adb", env!("CARGO_BIN_EXE_echo-setup-test-adb")])
        .output()?;
    assert_eq!(private_output(&doctor)?["auth_mode"], "api_key");
    Ok(())
}

#[test]
fn t3_api_key_refuses_chatgpt_configs_and_invalid_keys_with_fixed_guidance() -> Result {
    let (fixture, key) = api_key_fixture("chatgpt")?;
    let config = fixture.0.join("chatgpt.toml");
    fs::write(&config, include_str!("../../../../examples/bridge.echo.toml"))?;
    let local = fixture
        .install()
        .arg("--from-dir")
        .arg(fixture.bundle()?)
        .arg("--config")
        .arg(&config)
        .arg("--openai-api-key-file")
        .arg(&key)
        .output()?;
    assert_eq!(private_output(&local)?["error"], "api_key_config_mode_chatgpt");
    assert_eq!(fixture.ledger()?, Vec::<Vec<String>>::new());
    fs::write(&key, "\n")?;
    let invalid = fixture
        .install()
        .arg("--from-dir")
        .arg(fixture.bundle()?)
        .arg("--openai-api-key-file")
        .arg(&key)
        .output()?;
    assert_eq!(private_output(&invalid)?["error"], "api_key_file_invalid");
    assert_eq!(fixture.ledger()?, Vec::<Vec<String>>::new());
    fs::write(&key, KEY)?;
    let installed = fixture
        .install()
        .arg("--from-dir")
        .arg(fixture.bundle()?)
        .arg("--openai-api-key-file")
        .arg(&key)
        .output()?;
    assert_key_absent(&fixture, &installed);
    let result = private_output(&installed)?;
    assert_eq!(result["error"], "api_key_mode_required");
    assert!(result["fix"].as_str().is_some_and(|fix| fix.contains("--force")));
    assert!(!fixture.0.join("api-key-input").exists());
    let calls = fixture.ledger()?;
    assert!(
        !calls
            .iter()
            .filter_map(|args| args.last())
            .any(|call| call.contains("cat > ") || call.ends_with("--device ensure-signin"))
    );
    Ok(())
}
