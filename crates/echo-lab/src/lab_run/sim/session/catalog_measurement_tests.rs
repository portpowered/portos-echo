use std::{path::Path, process::Command};

fn fixture() -> Command {
    let mut command = Command::new("python3");
    command
        .arg("-B")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lab_run/sim/session/catalog_measurement/replay.py"));
    command
}

#[test]
fn catalog_measurement_unit() {
    assert!(fixture().arg("unit").status().unwrap().success());
}

#[test]
#[ignore = "T3: verified offline Linux app-server pin and loopback peers"]
fn t3_session_tool_catalog() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new("sh")
        .arg(root.join("scripts/codex-app-server.sh"))
        .env("ECHO_CODEX_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "offline verified pin unavailable: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let binary = std::str::from_utf8(&output.stdout).unwrap().trim();
    assert!(fixture().args(["replay", binary]).status().unwrap().success());
}
