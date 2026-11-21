//! T3 CLI witnesses through prebuilt production binaries; no build in tests.
use super::{Session, Value, json};

#[test]
#[ignore = "T3 requires Linux, openssl, prebuilt bridge/Muse binaries and pinned hassmic-host"]
fn m7_cli_both_cases_and_seeded_wrong_terminal_count_publish_exact_reports() {
    let bridge = std::path::Path::new(env!("CARGO_BIN_EXE_echo-lab")).with_file_name("echo-bridge");
    for seeded in [false, true] {
        let session = Session::new();
        let original = include_str!("../../../../tests/scenarios/m7-muse.yaml");
        let scenario = if seeded {
            original.replace("status: completed}", "status: completed, count: 2}")
        } else {
            original.into()
        };
        std::fs::write(session.0.join("scenario.yaml"), scenario).unwrap();
        let output = session.command(bridge.to_str().unwrap()).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(i32::from(seeded)),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = session.report();
        assert_eq!(report["result"], if seeded { "fail" } else { "pass" });
        assert_eq!(report["checks"]["world_closed"], true);
        assert_eq!(report["checks"]["step_1"], !seeded);
        check_cases(&session, &report, seeded).unwrap();
        if seeded {
            assert!(String::from_utf8_lossy(&output.stderr).contains("expected 2 occurrences, observed 1"));
        }
        for case in ["success", "no_report"] {
            assert!(!session.0.join(format!("artifacts/private-{case}")).exists());
        }
    }
}

fn check_cases(session: &Session, report: &Value, seeded: bool) -> Result<(), Box<dyn std::error::Error>> {
    let cases = report["observations"].as_array().ok_or("missing case observations")?;
    assert_eq!(cases.len(), if seeded { 1 } else { 2 });
    for (index, case) in cases.iter().enumerate() {
        check(case, index == 1);
        let name = format!("m7-{}-audible.json", if index == 1 { "no_report" } else { "success" });
        assert!(
            report["evidence"]
                .as_array()
                .is_some_and(|evidence| evidence.contains(&json!(name)))
        );
        let audio: Value = serde_json::from_slice(&std::fs::read(session.0.join("artifacts").join(name))?)?;
        check_audio(&audio);
    }
    Ok(())
}

fn check(case: &Value, negative: bool) {
    assert_eq!(case["case"], if negative { "no_report" } else { "success" });
    for field in [
        "delegation_count",
        "muse_submit_count",
        "terminal_count",
        "native_tool_call_count",
        "native_append_count",
        "spoken_reply_count",
        "gadget_submit_count",
    ] {
        assert_eq!(case[field], 1, "{field}");
    }
    for field in ["client_tool_call_count", "legacy_dispatch_count"] {
        assert_eq!(case[field], 0, "{field}");
    }
    assert_eq!(case["responses_request_count"], 2);
    check_terminal(case, negative);
}

fn check_terminal(case: &Value, negative: bool) {
    assert_eq!(case["completed_count"], usize::from(!negative));
    assert_eq!(case["failed_count"], usize::from(negative));
    assert_eq!(case["reply_pcm_bytes"], 9600);
    assert_eq!(case["report_attempt_count"], usize::from(!negative));
    assert_eq!(case["report_accepted_count"], usize::from(!negative));
    assert_eq!(
        case["report_task_id"],
        if negative { json!("") } else { case["task_id"].clone() }
    );
    assert_eq!(case["retryable"], if negative { json!(false) } else { Value::Null });
    assert_eq!(
        case["transcript"],
        if negative {
            "Muse did not report a result in time"
        } else {
            "Muse test job completed"
        }
    );
}

#[test]
#[ignore = "T3 requires prebuilt echo-lab and production bridge"]
fn m7_cli_missing_muse_binary_fails_before_starting_private_world() {
    let session = Session::new();
    std::fs::write(
        session.0.join("scenario.yaml"),
        include_str!("../../../../tests/scenarios/m7-muse.yaml"),
    )
    .unwrap();
    let lab = session.0.join("echo-lab");
    std::fs::copy(env!("CARGO_BIN_EXE_echo-lab"), &lab).unwrap();
    let bridge = std::path::Path::new(env!("CARGO_BIN_EXE_echo-lab")).with_file_name("echo-bridge");
    let output = std::process::Command::new(lab)
        .current_dir(&session.0)
        .args(["run", "scenario.yaml", "--driver", "sim", "--bridge"])
        .arg(bridge)
        .args(["--out", "artifacts"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "echo-lab: m7: missing prebuilt echo-muse-daemon; run cargo build --locked -p echo-bridge -p echo-muse-bridge -p echo-lab --bins\n"
    );
    assert_eq!(std::fs::read_dir(session.0.join("artifacts")).unwrap().count(), 1);
    assert!(session.0.join("artifacts/events.jsonl").is_file());
}

fn check_audio(case: &Value) {
    assert_eq!(case["audibility"]["pcm_bytes"], 9600);
    assert!(
        case["audibility"]["nonzero_samples"]
            .as_u64()
            .is_some_and(|count| count >= 2400)
    );
    assert!(
        case["audibility"]["audible_samples"]
            .as_u64()
            .is_some_and(|count| count >= 2400)
    );
    assert!(
        case["audibility"]["host"]["nonzero_bytes"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    let states = case["ui_states"]
        .as_array()
        .map(|states| states.iter().map(|state| state["state"].clone()).collect::<Vec<_>>());
    assert_eq!(
        states,
        Some(vec![
            json!("ready"),
            json!("listening"),
            json!("connected"),
            json!("thinking"),
            json!("speaking"),
            json!("connected"),
            json!("ready")
        ])
    );
}
