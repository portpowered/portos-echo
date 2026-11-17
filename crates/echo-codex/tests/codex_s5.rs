//! T3 public Rust client journey against the unmodified pin and local fakes.
#[path = "support/codex_journey.rs"]
mod journey;
use std::{error::Error, io::Write, path::PathBuf, process::Command};

#[test]
fn t3_pinned_codex_local_call_and_delegation() -> Result<(), Box<dyn Error>> {
    if let Ok(config) = std::env::var("ECHO_CODEX_JOURNEY") {
        return journey::run(&config);
    }
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
    std::thread::scope(|scope| {
        let jobs: Vec<_> = [
            "happy",
            "interrupt",
            "call-401",
            "call-503",
            "sideband-401",
            "responses-401",
        ]
        .into_iter()
        .map(|case| {
            let root = &root;
            let binary = binary.trim();
            scope.spawn(move || run_probe(root, binary, case).map_err(|error| error.to_string()))
        })
        .collect();
        for job in jobs {
            job.join().map_err(|_| "probe thread failed")??;
        }
        Ok::<_, Box<dyn Error>>(())
    })?;
    Ok(())
}

fn run_probe(root: &std::path::Path, binary: &str, case: &str) -> Result<(), Box<dyn Error>> {
    let output = Command::new(if cfg!(windows) { "python" } else { "python3" })
        .arg(root.join("crates/echo-codex/tests/support/codex_probe.py"))
        .arg(binary)
        .arg("--rust-client")
        .arg(std::env::current_exe()?)
        .args(["--case", case])
        .output()?;
    std::io::stdout().write_all(&output.stdout)?;
    assert!(
        output.status.success(),
        "probe: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("PUBLIC_CLIENT_PASS"));
    Ok(())
}
