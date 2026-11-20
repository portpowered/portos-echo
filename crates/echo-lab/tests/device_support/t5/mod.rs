//! T3 configured CLI journeys; all tools are synthetic and sockets loopback.
mod m4;
#[cfg(unix)]
mod m5;
mod session;
mod setup;
mod voice;
use crate::run_support::{FAKE, Session, failed};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn scenario() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/scenarios/m2-live-connect.yaml")
}

fn prepare(extra: &Value) -> Session {
    let mut fixture = json!({"real_config":true,"device_bridge_fault":"t5"});
    fixture
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let session = Session::new(&fixture);
    fixture["launch_record"] = session.0.join("launch.json").to_string_lossy().into_owned().into();
    fs::write(session.0.join("fixture.json"), fixture.to_string()).unwrap();
    let contents = include_str!("../../../../../examples/bridge.example.toml")
        .replace("echo.example.invalid", "127.0.0.1")
        .replace("0.0.0.0:16900", "127.0.0.1:0"); // lint-rules: allow-literal inert template substitution
    fs::write(session.0.join("bridge.toml"), contents).unwrap();
    session
}

fn command(session: &Session, real: bool) -> Command {
    let mut command = session.command();
    command.arg("run").arg(scenario()).args([
        "--driver",
        "device",
        "--bench",
        "ring",
        "--port",
        FAKE,
        "--adb",
        FAKE,
        "--stt",
        FAKE,
        "--python",
        FAKE,
        "--out",
        "artifacts",
        "--bridge",
        FAKE,
        "--bridge-config",
        "bridge.toml",
    ]);
    if real {
        command.args(["--live", "real"]);
    }
    command
}

fn observations(session: &Session) -> Vec<Value> {
    fs::read_to_string(session.0.join("artifacts/device-steps.jsonl"))
        .unwrap()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect()
}

#[test]
fn t3_device_t5_m2_authored_scenario_and_negative_disposition() {
    for real in [false, true] {
        let session = prepare(&json!({}));
        let output = command(&session, real).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(session.report()["result"], "pass");
        let rows = observations(&session);
        assert_eq!(rows[0]["live"], if real { "real" } else { "fake" });
        let skips: Vec<_> = rows.iter().filter(|row| row["event"] == "device.negative").collect();
        assert_eq!(skips.len(), 2);
        for (index, skip) in skips.iter().enumerate() {
            assert_eq!(skip["negative_index"], index);
            assert_eq!(skip["disposition"], "not run at T5 (T3/T4 cover it)");
        }
        assert_eq!(rows.iter().filter(|row| row["event"] == "device.ui").count(), 7);
        assert_eq!(session.operations().iter().filter(|op| *op == "bridge").count(), 1);
        assert_eq!(session.operations().iter().filter(|op| *op == "play").count(), 1);
        assert_eq!(session.operations().iter().filter(|op| *op == "release").count(), 1);
        assert!(!session.0.join("artifacts/configured.private").exists());
    }
}

#[test]
fn t3_device_t5_real_config_passthrough() {
    let session = prepare(&json!({}));
    let path = session.0.join("bridge.toml");
    let original = fs::read_to_string(&path).unwrap();
    let home = serde_json::to_string(&session.0.join("protected/home").to_string_lossy()).unwrap();
    let contents = format!("{original}\n[codex]\nhome = {home}\n");
    fs::write(&path, &contents).unwrap();
    let output = command(&session, true).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record: Value = serde_json::from_slice(&fs::read(session.0.join("launch.json")).unwrap()).unwrap();
    assert_eq!(record["args"][0], "--config");
    assert_eq!(record["args"][1], "bridge.toml");
    assert_eq!(record["cwd"], session.0.to_string_lossy().as_ref());
    assert_eq!(record["config"], contents);
    for flag in ["--live-loopback", "--model-loopback"] {
        assert!(!record["args"].as_array().unwrap().iter().any(|arg| arg == flag));
    }
    assert_eq!(fs::read_to_string(&path).unwrap(), contents);
    assert!(!session.0.join("protected").exists());
}

#[test]
fn t3_device_t5_screen_oracles_and_late_error_revoke_success() {
    for (fixture, error) in [
        (json!({"screen_fault":"auth"}), "screen: authentication failed"),
        (json!({"screen_fault":"stale"}), "screen: stale or missing presentation"),
        (
            json!({"screen_fault":"missing"}),
            "screen: stale or missing presentation",
        ),
        (
            json!({"screen_fault":"wrong"}),
            "screen expectation: latest UiState did not match",
        ),
        (json!({"late_error":true}), "device: bridge errors observed"),
    ] {
        let session = prepare(&fixture);
        let output = command(&session, true).output().unwrap();
        failed(&output, error);
        assert_eq!(session.report()["result"], "fail");
        assert!(!session.0.join("artifacts/configured.private").exists());
        if fixture["late_error"] == true {
            assert_eq!(session.report()["checks"]["bridge_errors_absent"], false);
        }
    }
}

#[test]
fn t3_device_t5_inline_secret_refusal_and_admission_guards() {
    let session = prepare(&json!({}));
    let path = session.0.join("bridge.toml");
    let contents = fs::read_to_string(&path)
        .unwrap()
        .replace("[live]", "[live]\nsecret = 'synthetic-inline'");
    fs::write(&path, contents).unwrap();
    let output = command(&session, true).output().unwrap();
    failed(&output, "device: inline secret in bridge configuration");
    assert_eq!(session.operations(), Vec::<String>::new());
    assert!(!session.0.join("artifacts").exists());
    for extra in [
        vec!["--live", "fake"],
        vec!["--live", "real"],
        vec!["--live", "real", "--live", "real"],
    ] {
        let output = session.run(&extra);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(session.operations(), Vec::<String>::new());
    }
}
