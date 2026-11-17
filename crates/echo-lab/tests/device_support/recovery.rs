//! P-3: unchanged scenario with actual independent process replacement.
use super::*;

#[test]
#[ignore = "T3 requires prebuilt bridge and pinned native host"]
fn device_m1_complete_unchanged_scenario_recovers_ten_plus_ten() {
    let bench = Bench::new();
    let scenario = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/scenarios/m1-hassmic-bridge.yaml");
    // Select the original file; keep the exact scenario bytes and digest.
    let command = bench.command(&bridge());
    let args: Vec<_> = command.get_args().map(std::ffi::OsStr::to_os_string).collect();
    let mut full = Command::new(command.get_program());
    full.current_dir(bench.root.path());
    for (name, value) in command.get_envs() {
        if let Some(value) = value {
            full.env(name, value);
        }
    }
    full.arg(&args[0]).arg(&scenario).args(&args[2..]);
    full.args(["--operator-confirm", "confirm"]);
    let (status, prompts, errors) = mute::operator(&bench, full, "mute");
    assert_eq!(errors, Vec::<String>::new());
    assert_eq!(status.code(), Some(0));
    assert_eq!(prompts.len(), 2);
    let report = bench.report();
    assert_eq!(report["result"], "pass");
    assert_eq!(report["checks"]["session_released"], true);
    assert_eq!(report["checks"]["bridge_errors_absent"], true);
    assert_measurements(&report);
    assert_recoveries(&bench);
}

fn assert_measurements(report: &Value) {
    let measured = &report["m1"];
    assert_eq!(measured["bridge_restarts"], 10);
    assert_eq!(measured["hassmic_host_restarts"], 10);
    assert_eq!(measured["ready_before_first_wake"], 1);
    assert_eq!(measured["mute_status"], "pass");
    assert_eq!(
        measured["mute_window"],
        json!({"received_frames":0,"forwarded_frames":0})
    );
    let wakes = measured["streamed_wakes"].as_array().unwrap();
    assert_eq!(wakes.len(), 22);
    let ids: std::collections::BTreeSet<_> = wakes
        .iter()
        .map(|v| {
            assert!(v["mic_frames"].as_u64().unwrap() >= 10);
            (v["bridge_generation"].as_u64().unwrap(), v["run_id"].as_str().unwrap())
        })
        .collect();
    assert_eq!(ids.len(), 22);
}

fn assert_recoveries(bench: &Bench) {
    let records = mute::observations(bench);
    let recovered: Vec<_> = records
        .iter()
        .filter(|r| r["event"] == "device.restart_recovered")
        .collect();
    assert_eq!(recovered.len(), 20);
    for (i, recovery) in recovered.iter().enumerate() {
        assert_eq!(recovery["iteration"], i % 10 + 1);
        assert_ne!(recovery["pid"], recovery["previous_pid"]);
        assert!(recovery["mic_frames"].as_u64().unwrap() >= 10);
    }
    assert_eq!(records.last().unwrap()["bridge_errors"], 0);
    mute::assert_mute_evidence(bench);
    assert!(!bench.root.path().join("artifacts/utterance-11.wav").exists());
    assert!(!bench.root.path().join("artifacts/utterance-12.wav").exists());
}

#[test]
#[ignore = "T3 requires prebuilt bridge and pinned native host"]
fn device_m1_recovery_rejects_ambiguous_targets_and_failed_kills_without_credit() {
    for fault in ["absent", "ambiguous", "kill"] {
        let bench = Bench::new();
        let mut fixture = json!({"real_config":true});
        match fault {
            "absent" => fixture["device_ps"] = json!("PID PPID NAME\n2 1 hassmic"),
            "ambiguous" => fixture["device_ps"] = json!("PID PPID NAME\n3 2 hassmic\n4 2 hassmic"),
            _ => fixture["device_fail_kill"] = json!(true),
        }
        fs::write(bench.root.path().join("fixture.json"), fixture.to_string()).unwrap();
        bench.scenario(
            &json!([{"restart":{"process":"hassmic-host","count":2,"say":"fixture.pcm"}},
            {"say":"fixture.pcm"}]),
        );
        let result = bench.command(&bridge()).output().unwrap();
        assert_eq!(result.status.code(), Some(1));
        assert_failed_recovery(&bench);
        let ledger: Vec<Value> = fs::read_to_string(bench.root.path().join("ledger.jsonl"))
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        let kills = ledger.iter().filter(|r| r["args"][1] == "kill").count();
        assert_eq!(kills, usize::from(fault == "kill"));
        assert!(ledger.iter().all(|r| r["operation"] != "play"));
    }
}

fn assert_failed_recovery(bench: &Bench) {
    let report = bench.report();
    assert_eq!(report["result"], "fail");
    assert_eq!(report["checks"]["step_0"], false);
    assert!(report["checks"].get("step_1").is_none());
    assert_eq!(report["checks"]["session_released"], true);
    assert_eq!(report["m1"]["hassmic_host_restarts"], 0);
    assert_eq!(report["m1"]["streamed_wakes"], json!([]));
    assert!(!bench.root.path().join("artifacts/configured.private").exists());
    assert!(!bench.root.path().join("artifacts/bridge.private.jsonl").exists());
    assert!(
        mute::observations(bench)
            .iter()
            .all(|r| r["event"] != "device.restart_recovered")
    );
}

#[test]
#[ignore = "T3 requires prebuilt bridge and pinned native host"]
fn device_m1_recovery_cancellation_retains_started_target_and_collects_owner() {
    let bench = Bench::new();
    fs::write(
        bench.root.path().join("fixture.json"),
        json!({"real_config":true,"device_cancel_kill":true}).to_string(),
    )
    .unwrap();
    bench.scenario(
        &json!([{"restart":{"process":"hassmic-host","count":2,"say":"fixture.pcm"}},
        {"say":"fixture.pcm"}]),
    );
    let mut command = bench.command(&bridge());
    command.args(["--cancel-file", "cancel.signal"]);
    let result = command.output().unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("run cancelled"));
    assert_failed_recovery(&bench);
    let records = mute::observations(&bench);
    let started: Vec<_> = records
        .iter()
        .filter(|r| r["event"] == "device.restart_started")
        .collect();
    assert_eq!(started.len(), 1);
    assert!(started[0]["target_pid"].as_u64().unwrap() > 1);
}

#[test]
#[ignore = "T3 requires prebuilt bridge and pinned native host"]
fn device_m1_recovery_does_not_claim_an_old_connection_after_noop_kill() {
    let bench = Bench::new();
    fs::write(
        bench.root.path().join("fixture.json"),
        json!({"real_config":true,"device_no_reconnect":true}).to_string(),
    )
    .unwrap();
    bench.scenario(
        &json!([{"restart":{"process":"hassmic-host","count":2,"say":"fixture.pcm"}},
        {"say":"fixture.pcm"}]),
    );
    let result = bench.command(&bridge()).output().unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("device: observation timed out"));
    assert_failed_recovery(&bench);
}

#[test]
#[ignore = "T3 controlled final-trace faults require pinned native host"]
fn device_m1_recovery_finalizes_original_generation_before_replacement() {
    for fault in ["error", "incomplete"] {
        let bench = Bench::new();
        fs::write(
            bench.root.path().join("fixture.json"),
            json!({"real_config":true,"device_bridge_fault":fault}).to_string(),
        )
        .unwrap();
        bench.scenario(&json!([{"restart":{"process":"bridge","count":2,"say":"fixture.pcm"}},
            {"say":"fixture.pcm"}]));
        let result = bench.command(std::path::Path::new(FAKE)).output().unwrap();
        assert_eq!(result.status.code(), Some(1));
        let report = bench.report();
        assert_eq!(report["result"], "fail");
        assert_eq!(report["checks"]["step_0"], false);
        assert!(report["checks"].get("step_1").is_none());
        let records = mute::observations(&bench);
        assert!(records.iter().all(|r| r["event"] != "device.restart_recovered"));
        if fault == "incomplete" {
            assert!(records.iter().all(|r| r["event"] != "device.summary"));
        } else {
            assert_eq!(records.last().unwrap()["bridge_errors"], 1);
            assert_eq!(report["checks"]["bridge_errors_absent"], false);
        }
        assert!(!bench.root.path().join("artifacts/configured.private").exists());
        assert!(!bench.root.path().join("artifacts/bridge.private.jsonl").exists());
    }
}
