//! T3: customer CLI consumes prebuilt bridge, pinned host and real paired SSE.
use super::*;

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host"]
fn m6_cli_orders_animation_return_and_exact_later_host_playback() {
    let session = Session::new();
    fs::write(
        session.0.join("scenario.yaml"),
        include_str!("../../../../tests/scenarios/m6-emotion.yaml"),
    )
    .unwrap();
    // Omit --bridge to protect the customer's production artifact selection.
    let output = Command::new(env!("CARGO_BIN_EXE_echo-lab"))
        .current_dir(&session.0)
        .args(["run", "scenario.yaml", "--driver", "sim", "--out", "artifacts"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = session.report();
    assert_eq!(report["result"], "pass");
    assert_eq!(
        report["checks"],
        json!({"step_0":true,"step_1":true,"step_2":true,"step_3":true,"world_closed":true})
    );
    let expected = json!([
        {"state":"ready","message":"Echo Live ready"}, {"state":"listening"},
        {"state":"connected"},
        {"state":"emotion","name":"happy","frame_ms":80,"frames":["(^_^)","(^o^)"]},
        {"state":"connected"}, {"state":"speaking"}, {"state":"ready","message":"Echo Live ready"}
    ]);
    let frames = report["m6"]["ui_states"].as_array().unwrap();
    assert_eq!(frames.len(), 7);
    let mut seq = 0;
    for (frame, state) in frames.iter().zip(expected.as_array().unwrap()) {
        assert_eq!(&frame["state"], state);
        let next = frame["seq"].as_u64().unwrap();
        assert!(next > seq);
        seq = next;
    }
    assert_eq!(report["m6"]["return_state"], json!({"state":"connected"}));
    for (key, value) in [
        ("frame_count", 2),
        ("frame_ms", 80),
        ("reply_pcm_bytes", 9600),
        ("host_pcm_bytes", 9600),
    ] {
        assert_eq!(report["m6"][key], value);
    }
    m6_audio(&session, &report);
    assert!(!session.0.join("artifacts/private-success").exists());
}

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host"]
fn m4_cli_both_cases_link_exact_audible_pcm_and_complete_screen_sequence() {
    let session = Session::new();
    fs::write(
        session.0.join("scenario.yaml"),
        include_str!("../../../../tests/scenarios/m4-spoof-tool.yaml"),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_echo-lab"))
        .current_dir(&session.0)
        .args(["run", "scenario.yaml", "--driver", "sim", "--out", "artifacts"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = session.report();
    assert_eq!(report["result"], "pass");
    assert!(report["checks"].as_object().unwrap().values().all(|v| v == true));
    for case in ["success", "http500"] {
        m4_audio(&session, &report, case);
    }
}

fn m6_audio(session: &Session, report: &Value) {
    assert!(report["m6"]["host_nonzero_bytes"].as_u64().unwrap() > 0);
    assert!(
        report["evidence"]
            .as_array()
            .unwrap()
            .contains(&json!("m6-success-audible.json"))
    );
    let audio: Value =
        serde_json::from_slice(&fs::read(session.0.join("artifacts/m6-success-audible.json")).unwrap()).unwrap();
    assert_eq!(audio["run_id"], "run-1");
    assert_eq!(audio["ui_states"], report["m6"]["ui_states"]);
    assert_eq!(audio["audibility"]["pcm_bytes"], 9600);
    assert!(audio["audibility"]["nonzero_samples"].as_u64().unwrap() >= 2400);
    assert!(audio["audibility"]["audible_samples"].as_u64().unwrap() >= 2400);
}

fn m4_audio(session: &Session, report: &Value, case: &str) {
    let name = format!("m4-{case}-audible.json");
    assert!(report["evidence"].as_array().unwrap().contains(&json!(name)));
    let audio: Value = serde_json::from_slice(&fs::read(session.0.join("artifacts").join(name)).unwrap()).unwrap();
    assert_eq!(audio["case"], case);
    assert_eq!(audio["run_id"], "run-1");
    assert_eq!(audio["audibility"]["pcm_bytes"], 9600);
    assert_eq!(audio["audibility"]["host"]["pcm_bytes"], 9600);
    assert!(audio["audibility"]["host"]["nonzero_bytes"].as_u64().unwrap() > 0);
    assert!(audio["audibility"]["nonzero_samples"].as_u64().unwrap() >= 2400);
    assert!(audio["audibility"]["audible_samples"].as_u64().unwrap() >= 2400);
    let frames = audio["ui_states"].as_array().unwrap();
    assert_eq!(frames.len(), 5);
    for (index, (frame, state)) in frames
        .iter()
        .zip(["ready", "listening", "connected", "speaking", "ready"])
        .enumerate()
    {
        assert_eq!(frame["seq"], index + 1);
        assert_eq!(frame["state"]["state"], state);
    }
}
