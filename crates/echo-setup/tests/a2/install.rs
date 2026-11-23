use super::*;

const STEPS: [&str; 4] = [
    "1. Verify the release and select one rooted Echo.",
    "2. Stage and install A2, then wait for reboot.",
    "3. Complete ChatGPT sign-in, then Port OS linking when required.",
    "4. Say Computer. Confirm a fresh wake and run doctor.",
];

fn retained() -> Result<Fixture> {
    let fixture = Fixture::new()?;
    fs::write(fixture.0.join("a2-installed"), "0.2.0\n")?;
    fs::write(fixture.0.join("process-user"), "nobody")?;
    fs::write(fixture.0.join("link-state"), "linked")?;
    let config =
        include_str!("../../../../examples/bridge.echo.toml").replace("/data/local/echo-bridge/", "/data/local/echo/");
    fs::write(fixture.0.join("setup-config"), config)?;
    let clock = std::time::UNIX_EPOCH.elapsed()?.as_secs();
    fs::write(
        fixture.0.join("doctor"),
        format!("hassmic=ok\negress=ok\nclock={clock}\ndisk_free=262144\nprocess_user=nobody\n"),
    )?;
    Ok(fixture)
}

#[test]
fn t3_a2_install_resumes_the_retained_release_and_runs_the_four_steps() -> Result {
    let fixture = retained()?;
    let output = fixture.install().arg("--from-dir").arg(fixture.bundle()?).output()?;
    let result = private_output(&output)?;
    assert_eq!(output.status.code(), Some(0), "{result}");
    let messages: Vec<_> = String::from_utf8(output.stdout)?
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(messages.len(), 5);
    for (event, step) in messages.iter().zip(STEPS) {
        assert_eq!(*event, serde_json::json!({"type":"instruction","message":step}));
    }
    assert_eq!(result["version"], "0.2.0");
    assert_eq!(result["setup"], "complete");
    assert_eq!(fs::read_to_string(fixture.0.join("forwards.json"))?, "[]");
    let calls = fixture.ledger()?;
    assert!(calls[1..].iter().all(|argv| argv[..2] == ["-s", "SYNTHETIC-DEVICE"]));
    let operations: Vec<_> = calls.iter().filter_map(|argv| argv.last()).collect();
    for forbidden in ["/runas", "--magiskpolicy", "--host link", "--host signin"] {
        assert!(!operations.iter().any(|command| command.contains(forbidden)));
    }
    let index = |operation: &str| operations.iter().position(|command| command.contains(operation));
    assert!(index("--host status").ok_or("missing status")? < index("--host ensure-signin").ok_or("missing account")?);
    assert!(index("--host confirm-check").ok_or("missing wake")? < index("--host doctor").ok_or("missing doctor")?);
    Ok(())
}

#[test]
fn t3_a2_install_rejects_unverified_owner_root_and_version_before_account_work() -> Result {
    for (file, value, expected) in [
        ("a2-installed", "bad version\n", "a2_owner_unverified"),
        ("not-root", "", "a2_root_required"),
        ("a2-installed", "0.1.0\n", "a2_version_mismatch"),
    ] {
        let fixture = retained()?;
        fs::write(fixture.0.join(file), value)?;
        let output = fixture.install().arg("--from-dir").arg(fixture.bundle()?).output()?;
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(private_output(&output)?["error"], expected);
        assert!(!fixture.ledger()?.iter().filter_map(|argv| argv.last()).any(
            |command| command.contains("--host stop")
                || command.contains("--host ensure-signin")
                || command.contains("--host confirm-cursor")
                || command.contains("--magiskpolicy")
        ));
    }
    Ok(())
}

#[test]
fn t3_a2_install_preserves_active_owner_and_drains_forwards_if_doctor_fails() -> Result {
    let fixture = retained()?;
    fs::write(fixture.0.join("doctor"), "hassmic=failed\n")?;
    let output = fixture.install().arg("--from-dir").arg(fixture.bundle()?).output()?;
    assert_eq!(output.status.code(), Some(1));
    let result = private_output(&output)?;
    assert_eq!(result["error"], "a2_doctor_failed");
    assert!(result["setup"].is_null());
    assert_eq!(fs::read_to_string(fixture.0.join("a2-installed"))?, "0.2.0\n");
    assert_eq!(fs::read_to_string(fixture.0.join("forwards.json"))?, "[]");
    Ok(())
}

#[tokio::test]
async fn t3_a2_install_resumes_signin_and_link_with_authorized_prompts_in_order() -> Result {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::task::LocalSet::new().run_until(prompted()),
    )
    .await?
}

async fn prompted() -> Result {
    use tokio::io::AsyncBufReadExt;
    let fixture = retained()?;
    let (mut child, screen, barrier) = install_setup::start_prompted(&fixture, true).await?;
    let mut lines = tokio::io::BufReader::new(child.stdout.take().ok_or("missing stdout")?).lines();
    for message in &STEPS[..3] {
        instruction(&mut lines, message).await?;
    }
    install_setup::both_prompts(&mut lines, &screen, &barrier, true).await?;
    instruction(&mut lines, STEPS[3]).await?;
    fs::write(
        fixture.0.join("confirm-evidence"),
        "wake=confirmed\nlink=connected\nrun=started\n",
    )?;
    let line = lines.next_line().await?.ok_or("missing success")?;
    assert_eq!(serde_json::from_str::<serde_json::Value>(&line)?["setup"], "complete");
    assert_eq!(lines.next_line().await?, None);
    let output = child.wait_with_output().await?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stderr, b"");
    assert_eq!(fs::read_to_string(fixture.0.join("forwards.json"))?, "[]");
    Ok(())
}

async fn instruction(
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    message: &str,
) -> Result {
    let line = lines.next_line().await?.ok_or("missing instruction")?;
    assert_eq!(serde_json::from_str::<serde_json::Value>(&line)?["message"], message);
    Ok(())
}

#[test]
fn t3_a2_install_keeps_the_api_key_private_and_skips_chatgpt_signin() -> Result {
    let fixture = retained()?;
    let config = fixture.0.join("setup-config");
    let contents = fs::read_to_string(&config)?.replace("mode = \"chatgpt\"", "mode = \"api_key\"");
    fs::write(&config, &contents)?;
    fs::write(
        fixture.0.join("api-key-status-reply"),
        "echo-install: api-key-status completed\n",
    )?;
    let key = fixture.0.join("local-key");
    fs::write(&key, "SYNTHETIC-PRIVATE-KEY\r\n")?;
    let output = fixture
        .install()
        .arg("--from-dir")
        .arg(fixture.bundle()?)
        .arg("--config")
        .arg(config)
        .arg("--force")
        .arg("--openai-api-key-file")
        .arg(key)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", private_output(&output)?);
    let commands: Vec<_> = fixture
        .ledger()?
        .into_iter()
        .filter_map(|argv| argv.last().cloned())
        .collect();
    let config = commands.iter().position(|cmd| cmd.ends_with("--host config-write"));
    let key = commands
        .iter()
        .position(|cmd| cmd.ends_with("--host api-key-write 'live.json'"));
    assert!(config.ok_or("configuration was not written")? < key.ok_or("API key was not written")?);
    assert!(
        !commands
            .iter()
            .any(|cmd| cmd.contains("signin") || cmd.contains("SYNTHETIC-PRIVATE-KEY"))
    );
    assert_eq!(
        fs::read_to_string(fixture.0.join("last-private-input"))?,
        "SYNTHETIC-PRIVATE-KEY"
    );
    assert!(!String::from_utf8(output.stdout)?.contains("SYNTHETIC-PRIVATE-KEY"));
    Ok(())
}

#[test]
fn t3_a2_install_explicit_legacy_keeps_the_existing_bundle_dispatch() -> Result {
    let fixture = Fixture::new()?;
    fs::write(fixture.0.join("link-state"), "linked")?;
    let output = fixture
        .install()
        .args(["--install-path", "legacy", "--from-dir"])
        .arg(fixture.bundle()?)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", private_output(&output)?);
    let commands: Vec<_> = fixture
        .ledger()?
        .into_iter()
        .filter_map(|args| args.last().cloned())
        .collect();
    assert!(commands.iter().any(|cmd| cmd.contains("--device install --bundle")));
    assert!(commands.iter().any(|cmd| cmd.ends_with("--device start")));
    assert!(commands.iter().any(|cmd| cmd.contains("--device confirm-check")));
    assert!(
        !commands
            .iter()
            .any(|cmd| cmd.contains("--host") || cmd.contains("--magiskpolicy"))
    );
    Ok(())
}

#[test]
fn t3_a2_install_refuses_wrong_running_identity_or_release_before_signin() -> Result {
    for (user, version, error) in [
        ("root", "0.2.0", "unsafe_process_user"),
        ("nobody", "0.1.0", "a2_version_mismatch"),
    ] {
        let fixture = retained()?;
        fs::write(
            fixture.0.join("status"),
            format!(
                "process=running\nprocess_user={user}\nuptime_seconds=0\nhassmic_link=unavailable\nversion_current={version}\nversion_previous=null\n"
            ),
        )?;
        let output = fixture.install().arg("--from-dir").arg(fixture.bundle()?).output()?;
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(private_output(&output)?["error"], error);
        assert!(!fs::read_to_string(fixture.0.join("ledger"))?.contains("--host ensure-signin"));
    }
    Ok(())
}

#[test]
fn t3_a2_install_incomplete_wake_exits_three_and_keeps_the_retained_install() -> Result {
    let fixture = retained()?;
    fs::write(
        fixture.0.join("confirm-evidence"),
        "wake=pending\nlink=pending\nrun=pending\n",
    )?;
    let output = fixture
        .install()
        .args(["--timeout-seconds", "3", "--from-dir"])
        .arg(fixture.bundle()?)
        .output()?;
    assert_eq!(output.status.code(), Some(3), "{}", private_output(&output)?);
    assert_eq!(private_output(&output)?["error"], "setup_timeout");
    assert_eq!(fs::read_to_string(fixture.0.join("a2-installed"))?, "0.2.0\n");
    assert_eq!(fs::read_to_string(fixture.0.join("forwards.json"))?, "[]");
    let calls = fs::read_to_string(fixture.0.join("ledger"))?;
    assert!(calls.contains("--host confirm-check"));
    assert!(!calls.contains("--host doctor"));
    Ok(())
}
