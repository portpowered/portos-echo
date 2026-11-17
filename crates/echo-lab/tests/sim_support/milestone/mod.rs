//! T3: public milestone command, production bridge and pinned native host.
use super::{Command, Output, Session, Value, fs, json};
use std::io::Write;
use std::path::PathBuf;

const SCENARIO: &str = include_str!("../../../../../tests/scenarios/m2-live-connect.yaml");

fn run(session: &Session, seeded: bool) -> (Output, PathBuf, Value) {
    let scenarios = session.0.join("tests/scenarios");
    let fixtures = session.0.join("tests/fixtures/lab");
    fs::create_dir_all(&scenarios).unwrap();
    fs::create_dir_all(&fixtures).unwrap();
    fs::write(
        scenarios.join("m2-live-connect.yaml"),
        if seeded {
            SCENARIO.replace("reason: idle, count: 1", "reason: idle, count: 2")
        } else {
            SCENARIO.into()
        },
    )
    .unwrap();
    fs::write(
        fixtures.join("w1-utterance.pcm"),
        include_bytes!("../../../../../tests/fixtures/lab/w1-utterance.pcm"),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_echo-lab"))
        .current_dir(&session.0)
        .env("TMP", &session.0)
        .env("TEMP", &session.0)
        .env("TMPDIR", &session.0)
        .args(["check", "--milestone", "M2"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(i32::from(seeded)), "{stdout}\n{stderr}");
    let result = if seeded { "fail" } else { "pass" };
    let path = PathBuf::from(stdout.trim().strip_suffix(&format!(": {result}")).unwrap());
    assert!(path.starts_with(&session.0), "{}", path.display());
    let report: Value = serde_json::from_slice(&fs::read(path.join("report.json")).unwrap()).unwrap();
    let schema: Value = serde_json::from_str(include_str!("../../../../../schemas/report.schema.json")).unwrap();
    assert!(jsonschema::validator_for(&schema).unwrap().is_valid(&report));
    assert_eq!(report["milestone"], "M2");
    assert_eq!(report["driver"], "sim");
    assert_eq!(report["tier"], "T3");
    assert_eq!(report["result"], result);
    // The seeded step stops before the remaining credential cases; M2 close
    // reports that incomplete scenario even though the executed world is reaped.
    assert_eq!(report["checks"]["world_closed"], !seeded);
    writeln!(
        std::io::stdout().lock(),
        "check --milestone M2: {stdout}stderr: {stderr}\nreport: {report}"
    )
    .unwrap();
    (output, path, report)
}

fn case(path: &std::path::Path, name: &str) -> Value {
    let measured: Value = serde_json::from_slice(&fs::read(path.join(format!("m2-{name}.json"))).unwrap()).unwrap();
    assert_eq!(measured["case"], name);
    assert_eq!(measured["provider_wire"], true);
    assert_eq!(measured["calls"], u64::from(name == "success"));
    assert_eq!(measured["refreshes"], u64::from(name == "expired"));
    let trace = measured["trace"].as_array().unwrap();
    let closes: Vec<_> = trace.iter().filter(|v| v["event"] == "live.closed").collect();
    assert_eq!(closes.len(), usize::from(name == "success"));
    let mut states = vec![
        json!({"state":"ready","message":"Echo Live ready"}),
        json!({"state":"listening"}),
    ];
    if name == "success" {
        assert_eq!(closes[0]["reason"], "idle");
        assert!(
            !trace
                .iter()
                .any(|v| v["event"] == "auth.relink_required" || v["event"] == "run.refused")
        );
        states.extend([
            json!({"state":"connected"}),
            json!({"state":"ready","message":"Echo Live ready"}),
        ]);
    } else {
        assert_eq!(measured["calls_after_detection"], 0);
        assert!(trace.iter().any(|v| v["event"] == "auth.relink_required"));
        assert!(
            trace
                .iter()
                .any(|v| v["event"] == "run.refused" && v["reason"] == "relink_required")
        );
        states.push(json!({"state":"error","message":"Run echo-bridge signin with your config, then restart the bridge.","relink":true}));
    }
    let frames: Vec<_> = states
        .iter()
        .enumerate()
        .map(|(i, state)| json!({"seq":i+1,"state":state}))
        .collect();
    assert_eq!(measured["frames"], json!(frames));
    writeln!(std::io::stdout().lock(), "m2-{name}.json: {measured}").unwrap();
    measured
}

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host"]
fn m2_milestone_publishes_exact_success_invalid_and_expired_results() {
    let session = Session::new();
    let (_, path, report) = run(&session, false);
    assert!(report["checks"].as_object().unwrap().values().all(|v| v == true));
    assert_eq!(
        report["evidence"],
        json!(["events.jsonl", "m2-success.json", "m2-invalid.json", "m2-expired.json"])
    );
    for name in ["success", "invalid", "expired"] {
        case(&path, name);
    }
}

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host"]
fn m2_milestone_real_oracle_failure_exits_nonzero_and_publishes_failure() {
    let session = Session::new();
    let (output, path, report) = run(&session, true);
    assert_eq!(report["checks"]["step_1"], false);
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("step 1:"), "{error}");
    assert!(error.contains("expected 2 occurrences, observed 1"), "{error}");
    assert!(!error.contains("unsupported milestone"));
    case(&path, "success");
}
