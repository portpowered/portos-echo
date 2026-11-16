use super::Session;
use serde_json::{Value, json};

fn scenario(session: &Session, name: &str, tiers: &[&str], steps: Value) {
    let root = session.0.join("tests/scenarios");
    std::fs::create_dir_all(&root).unwrap();
    let mut value = json!({"id":name,"milestone":"W1","tiers":tiers,"setup":{},"budgets":{},"negative":[]});
    value["steps"] = steps;
    std::fs::write(root.join(name), value.to_string()).unwrap();
}

fn command(session: &Session) -> std::process::Command {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_echo-lab"));
    command
        .current_dir(&session.0)
        .args(["run", "--all", "--driver", "sim"]);
    command
}

#[test]
fn suite_requires_an_executed_t3_and_preserves_not_runnable_without_a_pass() {
    let session = Session::new();
    scenario(&session, "m0.yaml", &["T0"], json!([]));
    std::fs::write(
        session.0.join("tests/scenarios/m4.yaml"),
        include_str!("../lab_m4.yaml"),
    )
    .unwrap();
    let output = command(&session).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "m4-spoof-tool: not-runnable (requires W2 assembled bridge/agent integration)\nexecuted=0 passed=0 failed=0 not_runnable=1\n"
    );
    for extra in [vec!["--unknown"], vec!["--python"], vec!["--driver", "sim"]] {
        assert_eq!(command(&session).args(extra).output().unwrap().status.code(), Some(1));
    }
}

#[test]
#[ignore = "T3 requires the prebuilt pinned hassmic-host"]
fn discovery_executes_w1_skips_other_tiers_and_reports_m4_without_passing_it() {
    let session = Session::new();
    scenario(
        &session,
        "w1.yaml",
        &["T3"],
        json!([{"press":"action"},{"expect_trace":["wyoming.run_started","wyoming.mic_frames {count_gte: 10}","wyoming.run_ended"]}]),
    );
    scenario(&session, "m0.yaml", &["T0"], json!([]));
    scenario(&session, "b0.yaml", &["T5"], json!([]));
    std::fs::write(
        session.0.join("tests/scenarios/m4.yaml"),
        include_str!("../lab_m4.yaml"),
    )
    .unwrap();
    let output = command(&session).args(["--out", "suite-root"]).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("m4-spoof-tool: not-runnable (requires W2 assembled bridge/agent integration)"));
    assert!(stdout.ends_with("executed=1 passed=1 failed=0 not_runnable=1\n"));
    let runs = std::fs::read_dir(session.0.join("suite-root/artifacts/W1"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(runs.len(), 1);
    let report: Value = serde_json::from_slice(&std::fs::read(runs[0].path().join("report.json")).unwrap()).unwrap();
    assert_eq!(report["result"], "pass");
    scenario(
        &session,
        "w1.yaml",
        &["T3"],
        json!([{"press":"action"},{"expect_trace":["absent"]}]),
    );
    let output = command(&session).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .ends_with("executed=1 passed=0 failed=1 not_runnable=1\n")
    );
}
