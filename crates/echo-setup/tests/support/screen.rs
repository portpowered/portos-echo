//! T3 screen orchestration plus real shell/Android-tool boundary on Linux.
use super::*;

#[test]
fn t3_screen_setup_precedes_accounts_and_refuses_before_runtime_on_failure() -> Result {
    for failed in [false, true] {
        let fixture = Fixture::new()?;
        let bundle = fixture.bundle()?;
        if failed {
            fs::write(fixture.0.join("screen-failed"), "")?;
        }
        let output = fixture.install().arg("--from-dir").arg(bundle).output()?;
        assert_eq!(output.status.code(), Some(i32::from(failed)));
        let result = private_output(&output)?;
        let ledger = fixture.ledger()?;
        let commands: Vec<_> = ledger.iter().filter_map(|args| args.last()).collect();
        let install = commands
            .iter()
            .position(|call| call.contains("--device install --bundle"))
            .ok_or("missing install")?;
        assert!(commands[install + 1].ends_with("/bundle/scripts/echo-install.sh' --device screen-setup"));
        assert_eq!(
            commands
                .iter()
                .any(|call| call.as_str() == "sh -s -- --device ensure-signin"),
            !failed
        );
        assert_eq!(fixture.0.join("process-user").exists(), !failed);
        assert!(
            commands
                .last()
                .is_some_and(|call| call.starts_with("rm -rf '/data/local/tmp/echo-setup-"))
        );
        if failed {
            assert_eq!(result["error"], "screen_setup_failed");
            assert!(!commands.iter().any(|call| call.contains("--device uninstall")));
        } else {
            assert_eq!(result["setup"], "complete");
        }
    }
    Ok(())
}

#[test]
fn t3_screen_failure_text_has_resumable_private_data_free_guidance() -> Result {
    let fixture = Fixture::new()?;
    let bundle = fixture.bundle()?;
    fs::write(fixture.0.join("screen-failed"), "")?;
    let output = fixture
        .command()
        .args(["install", "--adb", env!("CARGO_BIN_EXE_echo-setup-test-adb")])
        .arg("--from-dir")
        .arg(bundle)
        .output()?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, Vec::<u8>::new());
    let error = String::from_utf8(output.stderr)?;
    assert!(error.contains("Screen setup failed. Installed files remain."));
    assert!(error.contains("run install again"));
    assert!(!error.contains("SYNTHETIC-"));
    Ok(())
}

#[test]
#[cfg(unix)]
fn t3_shell_screen_installs_selects_home_and_recovers_partial_failure() -> Result {
    shell_witness("lifecycle")
}

#[test]
#[cfg(unix)]
fn t3_shell_screen_refuses_dependency_failures_and_resumes() -> Result {
    shell_witness("dependency")
}

#[test]
#[cfg(unix)]
fn t3_shell_screen_refuses_redirected_paths_before_android_mutation() -> Result {
    shell_witness("paths")
}

#[cfg(unix)]
fn shell_witness(group: &str) -> Result {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support/screen.py");
    let output = Command::new("python3").arg(script).arg(group).output()?;
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(
        String::from_utf8(output.stdout)?.trim(),
        format!("screen: {group}=PASS privacy=PASS")
    );
    Ok(())
}
