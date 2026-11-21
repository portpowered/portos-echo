//! T3: delivered CLI, common M5, fake device/bench and a loopback TLS issuer.
use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use p256::ecdsa::{Signature, SigningKey, signature::Verifier};
use std::{
    io::{BufRead, BufReader},
    os::unix::fs::PermissionsExt,
    process::{Child, Stdio},
};

const CONFIG: &str = include_str!("../../../../../examples/bridge.echo.toml");

struct Peer {
    session: Session,
    child: Child,
    issuer: String,
}

impl Peer {
    fn new(mode: &str) -> Self {
        let session = Session::new(&json!({}));
        let root = &session.0;
        let tool = root.join("tool");
        fs::write(&tool, include_str!("m5_peer.py")).unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(root.join("bridge.toml"), CONFIG).unwrap();
        fs::write(root.join("grant.json"), grant().to_string()).unwrap();
        fs::write(
            root.join("launcher.json"),
            json!({
                "private_dir":"/data/local/echo-bridge/private",
                "config":"/data/local/echo-bridge/private/lab-bridge.toml",
                "trace":"/data/local/echo-bridge/private/lab-trace.jsonl",
                "hassmic_log":"/data/local/hassmic/boot.log",
                "binary":"/data/local/echo-bridge/echo-bridge"
            })
            .to_string(),
        )
        .unwrap();
        let mut child = Command::new("python3")
            .arg(&tool)
            .args(["--serve"])
            .arg(root)
            .arg(mode)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        Self {
            session,
            child,
            issuer: ready["issuer"].as_str().unwrap().into(),
        }
    }

    fn run(&self) -> std::process::Output {
        let root = &self.session.0;
        let scenario = scenario().parent().unwrap().join("m5-port-os-link.yaml");
        self.session
            .command()
            .env("ECHO_M5_ROOT", root)
            .arg("run")
            .arg(scenario)
            .args([
                "--driver",
                "device",
                "--bench",
                "ring",
                "--live",
                "real",
                "--port",
                "./tool",
                "--adb",
                "./tool",
                "--stt",
                "./tool",
                "--python",
                "python3",
                "--bridge-config",
                "bridge.toml",
                "--device-bridge",
                "launcher.json",
                "--wake-clip",
                "wake.wav",
                "--cancel-file",
                "cancel",
                "--allow-quiet-hours",
                "--out",
                "artifacts",
            ])
            .output()
            .unwrap()
    }

    fn calls(&self) -> Vec<Vec<String>> {
        serde_json::from_slice(&fs::read(self.session.0.join("calls.json")).unwrap()).unwrap()
    }

    fn cleanup(&self, played: bool) {
        let calls = self.calls();
        assert_eq!(calls.iter().filter(|c| c[0] == "push").count(), 1);
        assert_eq!(
            calls.iter().filter(|c| c[0] == "forward" && c[1] == "--remove").count(),
            2
        );
        assert_eq!(
            calls.iter().filter(|c| c[0] == "session" && c[1] == "release").count(),
            usize::from(played)
        );
        assert!(!self.session.0.join("artifacts/configured.private").exists());
        self.preserved();
        for name in ["report.json", "events.jsonl", "device-steps.jsonl"] {
            let contents = fs::read_to_string(self.session.0.join("artifacts").join(name)).unwrap();
            for marker in [
                "PRIVATE-",
                "synthetic-consumed",
                "synthetic-rotated-",
                "private_key_jwk",
            ] {
                assert!(!contents.contains(marker), "{name} leaked {marker}");
            }
        }
        assert!(!serde_json::to_string(&calls).unwrap().contains("PRIVATE-"));
    }
    fn preserved(&self) {
        let remote = self.session.0.join("remote");
        assert_eq!(
            fs::read(remote.join("bridge.toml")).unwrap(),
            b"installed config sentinel\n"
        );
        assert_eq!(
            fs::read(remote.join("trace.jsonl")).unwrap(),
            b"installed trace sentinel\n"
        );
        assert_eq!(fs::read(remote.join("neighbor")).unwrap(), b"unowned neighbor\n");
        assert_eq!(fs::read_to_string(self.session.0.join("bridge.toml")).unwrap(), CONFIG);
        if !matches!(
            fs::read_to_string(self.session.0.join("mode")).unwrap().as_str(),
            "cleanup" | "stop"
        ) {
            for name in [
                "lab-bridge.toml",
                "lab-trace.jsonl",
                "lab-stdin",
                "lab-holder.pid",
                "lab-stdout.log",
                "lab-stderr.log",
                "lab-bridge.toml.lab-owner",
            ] {
                assert!(!remote.join(name).exists(), "owned residue: {name}");
            }
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn grant() -> Value {
    let key = SigningKey::from_slice(&[7; 32]).unwrap();
    let point = key.verifying_key().to_sec1_point(false);
    let config = echo_config::BridgeConfig::from_toml_str(CONFIG).unwrap();
    json!({"version":1,"issuer":"https://127.0.0.1:1","resource":config.mcp.server[0].url,
        "client_id":"dcr_synthetic_replay","private_key_jwk":{"kty":"EC","crv":"P-256",
        "x":B64.encode(&point.as_bytes()[1..33]),"y":B64.encode(&point.as_bytes()[33..65]),
        "d":B64.encode([7;32])},"grant":{"refresh_token":"synthetic-consumed"}})
}

fn replay(peer: &Peer) {
    let rows: Vec<Value> = serde_json::from_slice(&fs::read(peer.session.0.join("replay.json")).unwrap()).unwrap();
    assert_eq!(rows.len(), 1);
    let parts: Vec<_> = rows[0]["proof"].as_str().unwrap().split('.').collect();
    let key = SigningKey::from_slice(&[7; 32]).unwrap();
    key.verifying_key()
        .verify(
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &Signature::from_slice(&B64.decode(parts[2]).unwrap()).unwrap(),
        )
        .unwrap();
    let claims: Value = serde_json::from_slice(&B64.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(claims["htu"], format!("{}/token", peer.issuer));
    assert_eq!(claims["htm"], "POST");
    assert_eq!(rows[0]["form"]["refresh_token"], json!(["synthetic-consumed"]));
}

#[test]
fn t3_on_device_m5_common_scenario_dispatches_all_actions_and_measures_after_cleanup() {
    let peer = Peer::new("success");
    let output = peer.run();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = peer.session.report();
    assert_eq!(report["result"], "pass");
    assert_eq!(
        report["link_counts"],
        json!({"link":1,"tools_list":1,"tool_call":1,
        "forced_refresh":2,"replay_rejected":1,"relink_error":1,"restart_grant_kept":1,"unlink":1})
    );
    assert_eq!(report["checks"].as_object().unwrap().len(), 16);
    assert!(report["checks"].as_object().unwrap().values().all(|v| v == true));
    assert_eq!(fs::read_to_string(peer.session.0.join("pushed.toml")).unwrap(), CONFIG);
    assert_eq!(fs::read_to_string(peer.session.0.join("bridge.toml")).unwrap(), CONFIG);
    replay(&peer);
    peer.cleanup(true);
}

fn failed_journey(mode: &str) {
    let peer = Peer::new(mode);
    let output = peer.run();
    assert_eq!(output.status.code(), Some(1), "mode={mode}");
    let report = peer.session.report();
    assert_eq!(report["result"], "fail");
    if mode == "late" {
        assert_eq!(report["checks"]["step_13"], false);
    } else {
        assert_eq!(report["link_counts"], Value::Null);
    }
    if matches!(mode, "tool" | "unreturned" | "cancel" | "launch") {
        assert!(!peer.session.0.join("replay.json").exists());
    }
    if mode == "cancel" {
        // The command can queue audio before acknowledging its end timestamp.
        // That uncertain playback must block subsequent held commands, including
        // release, and name coordinator recovery rather than claiming cleanup.
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("playback completion unknown; coordinator recovery required")
        );
        assert_eq!(report["checks"]["session_released"], false);
    }
    peer.cleanup(!matches!(mode, "launch" | "cancel"));
}

#[test]
fn t3_on_device_m5_failed_result_revokes_counts() {
    failed_journey("tool");
}

#[test]
fn t3_on_device_m5_unreturned_call_revokes_counts() {
    failed_journey("unreturned");
}

#[test]
fn t3_on_device_m5_cancel_before_playback_ack_retains_hold_and_requires_recovery() {
    failed_journey("cancel");
}

#[test]
fn t3_on_device_m5_failed_launch_revokes_counts() {
    failed_journey("launch");
}

#[test]
fn t3_on_device_m5_failed_release_revokes_counts() {
    failed_journey("release");
}

#[test]
fn t3_on_device_m5_failed_cleanup_revokes_counts() {
    failed_journey("cleanup");
}

#[test]
fn t3_on_device_m5_late_receipt_rechecks_exact_counts() {
    failed_journey("late");
}

#[test]
fn t3_on_device_m5_stage_and_initial_launch_failures_preserve_installed_bytes() {
    for mode in ["push", "chown", "chmod", "mode", "owner", "initial-launch"] {
        let peer = Peer::new(mode);
        let output = peer.run();
        assert_eq!(output.status.code(), Some(1), "{mode}");
        let report = peer.session.report();
        assert_eq!(report["result"], "fail");
        assert_eq!(report["link_counts"], Value::Null);
        let remote = peer.session.0.join("remote");
        assert_eq!(
            fs::read(remote.join("bridge.toml")).unwrap(),
            b"installed config sentinel\n"
        );
        assert_eq!(
            fs::read(remote.join("trace.jsonl")).unwrap(),
            b"installed trace sentinel\n"
        );
        for name in [
            "lab-bridge.toml",
            "lab-trace.jsonl",
            "lab-stdin",
            "lab-holder.pid",
            "lab-stdout.log",
            "lab-stderr.log",
            "lab-bridge.toml.lab-owner",
        ] {
            assert!(!remote.join(name).exists(), "{mode} residue {name}");
        }
        assert!(!peer.calls().iter().any(|c| c[0] == "session" && c[1] == "hold"));
    }
}

#[test]
fn t3_on_device_m5_refuses_installed_empty_and_auxiliary_collisions_without_mutation() {
    refuses_collision("lab-bridge.toml");
}

#[test]
fn t3_on_device_m5_auxiliary_path_collision_trace() {
    refuses_collision("lab-trace.jsonl");
}

#[test]
fn t3_on_device_m5_auxiliary_path_collision_stdin() {
    refuses_collision("lab-stdin");
}

#[test]
fn t3_on_device_m5_auxiliary_path_collision_holder_pid() {
    refuses_collision("lab-holder.pid");
}

#[test]
fn t3_on_device_m5_auxiliary_path_collision_stdout() {
    refuses_collision("lab-stdout.log");
}

#[test]
fn t3_on_device_m5_auxiliary_path_collision_stderr() {
    refuses_collision("lab-stderr.log");
}

fn refuses_collision(name: &str) {
    for bytes in [b"foreign bytes".as_slice(), b""] {
        let peer = Peer::new("success");
        let root = &peer.session.0;
        let remote = root.join("remote");
        fs::write(remote.join(name), bytes).unwrap();
        if name == "lab-stdin" && !bytes.is_empty() {
            // An existing file forces mkfifo to fail through the same fake adb transport.
            let refusal = Command::new(root.join("tool"))
                .env("ECHO_M5_ROOT", root)
                .args([
                    "shell",
                    concat!(
                        "/data/local/hassmic/runas nobody 3990,inet ",
                        "mkfifo /data/local/echo-bridge/private/lab-stdin"
                    ),
                ])
                .output()
                .unwrap();
            assert_eq!(refusal.status.code(), Some(1));
            assert_eq!(refusal.stdout, b"");
            assert_eq!(refusal.stderr, b"", "{}", String::from_utf8_lossy(&refusal.stderr));
        }
        let output = peer.run();
        assert_eq!(output.status.code(), Some(1), "{name}");
        assert_eq!(fs::read(remote.join(name)).unwrap(), bytes);
        assert_eq!(
            fs::read(remote.join("bridge.toml")).unwrap(),
            b"installed config sentinel\n"
        );
        let report = peer.session.report();
        assert_eq!(report["result"], "fail");
        assert_eq!(report["link_counts"], Value::Null);
        let calls = peer.calls();
        assert!(!calls.iter().any(|c| c.join(" ").contains("setsid")));
        assert!(!calls.iter().any(|c| c.join(" ").contains("kill $(cat")));
    }
}

#[test]
fn t3_on_device_m5_failed_stop_retains_owned_residue_and_preserves_installed_bytes() {
    failed_journey("stop");
}

#[test]
fn t3_on_device_m5_nominated_installed_config_is_never_overwritten_or_removed() {
    for bytes in [b"installed config sentinel\n".as_slice(), b""] {
        let peer = Peer::new("success");
        let remote = peer.session.0.join("remote");
        let installed = remote.join("bridge.toml");
        fs::write(&installed, bytes).unwrap();
        let path = peer.session.0.join("launcher.json");
        let mut launcher: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        launcher["config"] = json!("/data/local/echo-bridge/private/bridge.toml");
        fs::write(path, launcher.to_string()).unwrap();
        let output = peer.run();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(fs::read(installed).unwrap(), bytes);
        assert_eq!(
            fs::read(remote.join("trace.jsonl")).unwrap(),
            b"installed trace sentinel\n"
        );
        assert_eq!(peer.session.report()["link_counts"], Value::Null);
        assert!(
            !peer
                .calls()
                .iter()
                .any(|c| c.join(" ").contains("setsid") || c.join(" ").contains("kill $(cat"))
        );
    }
}

#[test]
fn t3_on_device_m5_cancellation_before_stage_after_acquisition_and_during_start_preserves_bytes() {
    for mode in ["cancel-before", "cancel-stage", "cancel-start"] {
        let peer = Peer::new(mode);
        if mode == "cancel-before" {
            fs::write(peer.session.0.join("cancel"), "cancel").unwrap();
        }
        let output = peer.run();
        assert_eq!(output.status.code(), Some(1), "{mode}");
        assert_eq!(peer.session.report()["result"], "fail");
        if mode == "cancel-before" {
            assert_eq!(
                peer.session.report()["link_counts"],
                json!({"link":0,"tools_list":0,"tool_call":0,
                "forced_refresh":0,"replay_rejected":0,"relink_error":0,"restart_grant_kept":0,"unlink":0})
            );
        } else {
            assert_eq!(peer.session.report()["link_counts"], Value::Null);
        }
        let remote = peer.session.0.join("remote");
        assert_eq!(
            fs::read(remote.join("bridge.toml")).unwrap(),
            b"installed config sentinel\n"
        );
        assert_eq!(fs::read_dir(remote).unwrap().count(), 3, "{mode} left owned residue");
        if mode == "cancel-before" {
            assert!(
                !peer
                    .calls()
                    .iter()
                    .any(|c| c[0] == "push" || c.join(" ").contains("rm -f"))
            );
        }
    }
}

#[test]
fn t3_on_device_m5_failed_forward_release_still_cleans_owned_files_and_revokes_counts() {
    failed_journey("forward");
}
