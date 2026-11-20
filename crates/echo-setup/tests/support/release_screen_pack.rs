//! T3 native installer and delivered-shell pack lifecycle on Linux.
use super::{Command, PathBuf, Result};

fn run(case: &str) -> Result {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/test_release_screen_pack.py");
    let output = Command::new("python3")
        .arg(script)
        .arg(env!("CARGO_BIN_EXE_echo-setup"))
        .arg(case)
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        stdout.lines().last().ok_or("missing pack witness result")?,
        format!("release screen pack: {case}=PASS")
    );
    Ok(())
}

#[test]
fn default_and_fresh_sample() -> Result {
    run("fresh")
}

#[test]
fn legacy_and_retained_settings() -> Result {
    run("retained")
}

#[test]
fn migrates_root_owned_home() -> Result {
    run("migrate")
}

#[test]
fn unsafe_member() -> Result {
    run("unsafe")
}

#[test]
fn equal_existing_identity() -> Result {
    run("same")
}

#[test]
fn conflicting_existing_identity() -> Result {
    run("conflict")
}

#[test]
fn fallback_refusals() -> Result {
    run("refusals")
}

#[test]
fn offline_update_and_rollback() -> Result {
    run("offline")
}
