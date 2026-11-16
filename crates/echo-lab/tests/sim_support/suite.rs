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
    let mut command = session.cli();
    command.current_dir(&session.0).args(["--all", "--driver", "sim"]);
    command
}

#[test]
fn suite_requires_an_executed_t3_and_rejects_invalid_options() {
    let session = Session::new();
    scenario(&session, "m0.yaml", &["T0"], json!([]));
    let output = command(&session).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "executed=0 passed=0 failed=0 not_runnable=0\n"
    );
    for extra in [vec!["--unknown"], vec!["--python"], vec!["--driver", "sim"]] {
        assert_eq!(command(&session).args(extra).output().unwrap().status.code(), Some(1));
    }
}

fn discovery_session() -> Session {
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
    session
}

#[test]
#[ignore = "T3 requires the prebuilt pinned hassmic-host"]
fn discovery_executes_w1_and_both_m4_cases_and_skips_other_tiers() {
    // Each complete --all journey retains its serial scenario execution. The
    // success and seeded-failure invocations have independent private worlds,
    // so pair them instead of accumulating six host launches under one ceiling.
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let session = discovery_session();
            let output = command(&session).args(["--out", "suite-root"]).output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(0),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let stdout = String::from_utf8(output.stdout).unwrap();
            assert!(!stdout.contains("not-runnable"));
            assert!(stdout.ends_with("executed=2 passed=2 failed=0 not_runnable=0\n"));
            let runs = std::fs::read_dir(session.0.join("suite-root/artifacts/W1"))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(runs.len(), 1);
            let report: Value =
                serde_json::from_slice(&std::fs::read(runs[0].path().join("report.json")).unwrap()).unwrap();
            assert_eq!(report["result"], "pass");
        });
        scope.spawn(|| {
            let session = discovery_session();
            scenario(
                &session,
                "w1.yaml",
                &["T3"],
                json!([{"press":"action"},{"expect_trace":["absent"]}]),
            );
            let output = command(&session).output().unwrap();
            assert_eq!(output.status.code(), Some(1));
            let stdout = String::from_utf8(output.stdout).unwrap();
            assert!(
                stdout.ends_with("executed=2 passed=1 failed=1 not_runnable=0\n"),
                "{stdout}"
            );
        });
    });
}

fn w1_copy(session: &Session, threshold: u64) {
    let root = session.0.join("tests");
    std::fs::create_dir_all(root.join("scenarios")).unwrap();
    std::fs::create_dir_all(root.join("fixtures/lab")).unwrap();
    std::fs::write(
        root.join("scenarios/w1.yaml"),
        include_str!("../../../../tests/scenarios/w1-wake-stream.yaml")
            .replace("count_gte: 10", &format!("count_gte: {threshold}")),
    )
    .unwrap();
    std::fs::write(
        root.join("fixtures/lab/w1-utterance.pcm"),
        include_bytes!("../../../../tests/fixtures/lab/w1-utterance.pcm"),
    )
    .unwrap();
}

fn single(session: &Session) -> std::process::Command {
    let mut command = session.cli();
    command
        .current_dir(&session.0)
        .args(["tests/scenarios/w1.yaml", "--driver", "sim", "--out", "artifacts"]);
    command
}

fn observed_run(session: &Session) -> Vec<Value> {
    let report = session.report();
    assert!(report["evidence"].as_array().unwrap().contains(&json!("events.jsonl")));
    let events = std::fs::read_to_string(session.0.join("artifacts/events.jsonl")).unwrap();
    let run: Vec<Value> = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["event"].as_str().unwrap().starts_with("wyoming."))
        .collect();
    assert_eq!(run.len(), 3);
    for (record, name) in run
        .iter()
        .zip(["wyoming.run_started", "wyoming.mic_frames", "wyoming.run_ended"])
    {
        assert_eq!(record["event"], name);
        assert_eq!(record["run_id"], run[0]["run_id"]);
        assert_ne!(record["run_id"].as_str().unwrap(), "");
    }
    assert!(
        run.windows(2)
            .all(|pair| pair[0]["elapsed_ms"].as_u64().unwrap() <= pair[1]["elapsed_ms"].as_u64().unwrap())
    );
    run
}

#[test]
#[ignore = "T3 requires the prebuilt pinned hassmic-host"]
fn single_default_bridge_observes_real_frames_and_rejects_impossible_threshold() {
    for (threshold, exit, result) in [(10, 0, "pass"), (1_000_000, 1, "fail")] {
        let session = Session::new();
        w1_copy(&session, threshold);
        let output = single(&session).output().unwrap();
        let error = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(exit), "{error}");
        assert!(String::from_utf8_lossy(&output.stdout).ends_with(&format!(": {result}\n")));
        let report = session.report();
        assert_eq!(report["result"], result);
        assert_eq!(report["checks"]["step_1"], exit == 0);
        assert_eq!(report["checks"]["world_closed"], true);
        let run = observed_run(&session);
        assert!((10..1_000_000).contains(&run[1]["count"].as_u64().unwrap()));
        if exit == 1 {
            assert!(
                error.contains("trace expectation 1") && error.contains("wyoming.mic_frames"),
                "{error}"
            );
            assert!(error.contains("expected 1 occurrences, observed 0"), "{error}");
        }
        session.endpoints();
    }
}

#[test]
#[ignore = "T3 requires the prebuilt pinned hassmic-host"]
fn single_explicit_bridge_overrides_available_sibling() {
    let session = Session::new();
    session.scenario(
        json!([{"expect_trace":["wyoming.run_started","wyoming.mic_frames {count_gte: 10}","wyoming.run_ended"]}]),
        json!({}),
    );
    let bridge = session.0.join(if cfg!(windows) {
        "override bridge.exe"
    } else {
        "override bridge"
    });
    std::fs::copy(env!("CARGO_BIN_EXE_echo-lab-sim-script"), &bridge).unwrap();
    let mut records = super::records();
    for record in records.as_array_mut().unwrap() {
        record["run_id"] = json!("explicit-override");
    }
    std::fs::write(session.0.join("script.json"), json!({"records":records}).to_string()).unwrap();
    let output = session.command(bridge.to_str().unwrap()).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(session.report()["result"], "pass");
    assert_eq!(observed_run(&session)[0]["run_id"], "explicit-override");
    session.endpoints();
}

#[test]
fn single_missing_selected_bridge_fails_before_artifacts_or_world_start() {
    let session = Session::new();
    w1_copy(&session, 10);
    let cli = session.0.join(if cfg!(windows) { "echo-lab.exe" } else { "echo-lab" });
    std::fs::copy(env!("CARGO_BIN_EXE_echo-lab"), &cli).unwrap();
    let missing = cli.with_file_name(if cfg!(windows) {
        "echo-lab-sim-bridge.exe"
    } else {
        "echo-lab-sim-bridge"
    });
    let output = std::process::Command::new(&cli)
        .current_dir(&session.0)
        .args([
            "run",
            "tests/scenarios/w1.yaml",
            "--driver",
            "sim",
            "--out",
            "artifacts",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains(missing.to_str().unwrap().trim_start_matches(r"\\?\")),
        "{error}"
    );
    assert!(error.contains("cargo build -p echo-lab --bins --locked"), "{error}");
    for selected in ["./absent bridge", "echo-lab-sim-bridge"] {
        let output = single(&session).args(["--bridge", selected]).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("--bridge requires an explicit executable path") && error.contains(selected),
            "{error}"
        );
    }
    assert!(!session.0.join("artifacts").exists());
    assert!(!session.0.join("endpoints.json").exists());
    assert!(!session.0.join("cancel").exists());
}
