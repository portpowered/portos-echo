//! P-2: operator substitute controls only the native host key and confirmation file.
use super::*;
const PROMPT: &str = "Press the mute button, then create the operator confirmation file.";

fn mute_steps() -> Value {
    json!([
        {"expect_screen":{"state":"ready","message":"Echo Live ready"}},
        {"say":"fixture.pcm"},
        {"press":"mute"}, {"expect_screen":{"state":"muted"}},
        {"say":"fixture.pcm"}, {"press":"mute"},
        {"expect_screen":{"state":"ready","message":"Echo Live ready"}},
        {"expect_trace":["mic.mute_window {window_id: mute-1, received_frames: 0, forwarded_frames: 0}"]},
        {"say":"fixture.pcm"}
    ])
}

fn observations(bench: &Bench) -> Vec<Value> {
    fs::read_to_string(bench.root.path().join("artifacts/device-steps.jsonl"))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}

fn operator(
    bench: &Bench,
    mut command: Command,
    response: &str,
) -> (std::process::ExitStatus, Vec<String>, Vec<String>) {
    command.stderr(Stdio::piped()).stdout(Stdio::null());
    let mut child = command.spawn().unwrap();
    let mut prompts = Vec::new();
    let mut errors = Vec::new();
    for line in BufReader::new(child.stderr.take().unwrap()).lines() {
        let line = line.unwrap();
        if line == PROMPT {
            prompts.push(line);
            if response == "cancel" {
                fs::write(bench.root.path().join("cancel.signal"), "").unwrap();
            } else {
                if response == "mute" {
                    let mut stream = TcpStream::connect(bench.control).unwrap();
                    writeln!(stream, "{}", json!({"action":"mute"})).unwrap();
                    let mut response = String::new();
                    BufReader::new(stream).read_line(&mut response).unwrap();
                    assert_eq!(response.trim(), "ok");
                }
                fs::write(bench.root.path().join("confirm"), "").unwrap();
            }
        } else {
            errors.push(line);
        }
    }
    (child.wait().unwrap(), prompts, errors)
}

#[test]
#[ignore = "T3 requires prebuilt bridge and pinned native host"]
fn device_m1_mute_requires_two_fresh_confirmations_and_measures_zero_window() {
    let bench = Bench::new();
    bench.scenario(&mute_steps());
    let mut command = bench.command(&bridge());
    command.args(["--operator-confirm", "confirm"]);
    // A stale file is discarded before the first prompt; each prompt creates its own.
    fs::write(bench.root.path().join("confirm"), "stale").unwrap();
    let (status, prompts, errors) = operator(&bench, command, "mute");
    assert_eq!(errors, Vec::<String>::new());
    assert_eq!(status.code(), Some(0));
    assert_eq!(prompts, vec![PROMPT, PROMPT]);
    let report = bench.report();
    assert_eq!(report["result"], "pass");
    assert_eq!(report["checks"]["session_released"], true);
    let wakes = report["observations"].as_array().unwrap();
    assert_eq!(wakes.len(), 2);
    assert_ne!(wakes[0]["run_id"], wakes[1]["run_id"]);
    assert!(wakes.iter().all(|v| v["count"].as_u64().unwrap() >= 10));
    assert_mute_evidence(&bench);
}

#[test]
#[ignore = "T3 requires prebuilt bridge and pinned native host"]
fn device_m1_confirmation_missing_fails_exactly_and_executes_no_later_step() {
    let bench = Bench::new();
    bench.scenario(&json!([{"press":"mute"}, {"say":"fixture.pcm"}]));
    let result = bench.command(&bridge()).output().unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(result.stderr).unwrap().replace("\r\n", "\n"),
        format!("{PROMPT}\necho-lab: operator step not confirmed\n")
    );
    let report = bench.report();
    assert_eq!(report["result"], "fail");
    assert_eq!(report["checks"]["step_0"], false);
    assert!(report["checks"].get("step_1").is_none());
    assert_eq!(report["checks"]["session_released"], true);
    assert_eq!(report["observations"], json!([]));
    assert!(!bench.root.path().join("artifacts/configured.private").exists());
    assert!(!bench.root.path().join("artifacts/bridge.private.jsonl").exists());
}

#[test]
#[ignore = "T3 requires prebuilt bridge and pinned native host"]
fn device_m1_confirmation_cancel_collects_resources_without_later_step() {
    let bench = Bench::new();
    bench.scenario(&json!([{"press":"mute"}, {"say":"fixture.pcm"}]));
    let mut command = bench.command(&bridge());
    command.args(["--operator-confirm", "confirm", "--cancel-file", "cancel.signal"]);
    let (status, prompts, errors) = operator(&bench, command, "cancel");
    assert_eq!(errors, vec!["echo-lab: run cancelled"]);
    assert_eq!(status.code(), Some(1));
    assert_eq!(prompts, vec![PROMPT]);
    let report = bench.report();
    assert_eq!(report["result"], "fail");
    assert_eq!(report["checks"]["step_0"], false);
    assert!(report["checks"].get("step_1").is_none());
    assert_eq!(report["checks"]["session_released"], true);
    assert!(!bench.root.path().join("artifacts/configured.private").exists());
}

fn assert_mute_evidence(bench: &Bench) {
    let report = bench.report();
    let records = observations(bench);
    let confirms: Vec<_> = records
        .iter()
        .filter(|v| v["event"] == "device.operator_confirmed")
        .collect();
    assert_eq!(confirms.len(), 2);
    for (index, confirm) in confirms.iter().enumerate() {
        assert_eq!(confirm["press"], index + 1);
        assert!(confirm["confirmed_after_ms"].as_u64().unwrap() < 120_000);
    }
    let windows: Vec<_> = records.iter().filter(|v| v["event"] == "device.mute_window").collect();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0]["window_id"], "mute-1");
    assert_eq!(windows[0]["received_frames"], 0);
    assert_eq!(windows[0]["forwarded_frames"], 0);
    assert!(!bench.root.path().join("confirm").exists());
    assert!(!bench.root.path().join("artifacts/configured.private").exists());
    assert!(!bench.root.path().join("artifacts/bridge.private.jsonl").exists());
    assert_eq!(report["checks"]["bridge_errors_absent"], true);
}

#[test]
#[ignore = "T3 controlled observer faults require pinned local host"]
fn device_m1_confirmation_alone_cannot_prove_missing_ui_or_mute_measurement() {
    for fault in ["missing_muted", "missing_window"] {
        let bench = Bench::new();
        fs::write(
            bench.root.path().join("fixture.json"),
            json!({"real_config":true,"device_bridge_fault":fault}).to_string(),
        )
        .unwrap();
        bench.scenario(&json!([
            {"press":"mute"}, {"press":"mute"},
            {"expect_trace":["mic.mute_window {window_id: mute-1, received_frames: 0, forwarded_frames: 0}"]},
            {"say":"fixture.pcm"}
        ]));
        let mut command = bench.command(std::path::Path::new(FAKE));
        command.args(["--operator-confirm", "confirm"]);
        let (status, prompts, errors) = operator(&bench, command, "confirm");
        assert_eq!(status.code(), Some(1));
        let step = if fault == "missing_muted" { 0 } else { 2 };
        assert_eq!(prompts.len(), if step == 0 { 1 } else { 2 });
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains(if step == 0 {
            "screen: stream ended"
        } else {
            "expected 1 measurement, observed 0"
        }));
        let report = bench.report();
        assert_eq!(report["result"], "fail");
        assert_eq!(report["checks"][format!("step_{step}")], false);
        assert!(report["checks"].get("step_3").is_none());
        assert_eq!(report["checks"]["session_released"], true);
        assert_eq!(report["observations"], json!([]));
        assert!(!bench.root.path().join("artifacts/configured.private").exists());
    }
}
