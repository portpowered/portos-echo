//! T3 component feasibility proof. This uses the real pin and a raw stdio probe;
//! it does not prove the future Rust supervisor/client's lifecycle or bounds.
use std::{error::Error, io::Write, path::PathBuf, process::Command};

#[test]
fn t3_pinned_codex_local_call_and_delegation() -> Result<(), Box<dyn Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut helper = if cfg!(windows) {
        let mut command = Command::new("powershell.exe");
        command.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]);
        command.arg(root.join("scripts/codex-app-server.ps1"));
        command
    } else {
        let mut command = Command::new("sh");
        command.arg(root.join("scripts/codex-app-server.sh"));
        command
    };
    // Provision outside Cargo before testing: no downloader networking in T3.
    helper.env("ECHO_CODEX_OFFLINE", "1");
    let resolved = helper.output()?;
    assert!(
        resolved.status.success(),
        "helper: {}",
        String::from_utf8_lossy(&resolved.stderr)
    );
    let binary = String::from_utf8(resolved.stdout)?;
    let output = Command::new(if cfg!(windows) { "python" } else { "python3" })
        .arg(root.join("crates/echo-codex/tests/support/codex_probe.py"))
        .arg(binary.trim())
        .output()?;
    std::io::stdout().write_all(&output.stdout)?;
    assert!(
        output.status.success(),
        "probe: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("S5_PASS"));
    Ok(())
}
