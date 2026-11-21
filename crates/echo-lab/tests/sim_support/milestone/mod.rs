//! T3: public milestone command, production bridge and pinned native host.
use super::{Command, Output, Session, Value, fs, json};
use std::io::Write;
use std::path::PathBuf;

// Codex performs two startup refreshes (first measured at rust-v0.160.0); a release that changes the count fails here.
const EXPIRED_REFRESHES: u64 = 2;

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
    assert_eq!(
        measured["refreshes"],
        if name == "expired" { EXPIRED_REFRESHES } else { 0 }
    );
    let trace = measured["trace"].as_array().unwrap();
    let closes: Vec<_> = trace.iter().filter(|v| v["event"] == "live.closed").collect();
    assert_eq!(closes.len(), usize::from(name == "success"));
    let mut states = vec![
        json!({"state":"ready","message":"Echo Live ready"}),
        json!({"state":"listening"}),
    ];
    if name == "success" {
        audible_reply(&measured["reply_pcm"]);
        assert_eq!(closes[0]["reason"], "idle");
        assert!(
            !trace
                .iter()
                .any(|v| v["event"] == "auth.relink_required" || v["event"] == "run.refused")
        );
        states.extend([
            json!({"state":"connected"}),
            json!({"state":"speaking"}),
            json!({"state":"connected"}),
            json!({"state":"ready","message":"Echo Live ready"}),
            json!({"state":"ready","message":"Echo Live ready"}),
        ]);
    } else {
        states.clear();
        rejection(&measured, trace);
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

fn rejection(measured: &Value, trace: &[Value]) {
    for field in ["wyoming_admissions", "sidebands", "external"] {
        assert_eq!(measured[field], 0);
    }
    assert_eq!(measured["exit_code"], 1);
    assert_eq!(measured["stdout"], "");
    assert_eq!(measured["stderr"], "echo-bridge: authorization\n");
    assert_eq!(measured["auth_unchanged"], true);
    assert_eq!(measured["home_released"], true);
    assert_eq!(trace.iter().filter(|v| v["event"] == "auth.relink_required").count(), 1);
    for event in [
        "bridge.ready",
        "run.started",
        "live.connected",
        "mic.completed",
        "reply.played",
        "run.refused",
    ] {
        assert_eq!(trace.iter().filter(|v| v["event"] == event).count(), 0);
    }
}

fn audible_reply(audio: &Value) {
    assert_eq!(audio["pcm_bytes"], 9600);
    assert_eq!(audio["host"]["pcm_bytes"], 9600);
    assert!(audio["nonzero_samples"].as_u64().unwrap() >= 2400);
    assert!(audio["audible_samples"].as_u64().unwrap() >= 2400);
    assert!(audio["host"]["nonzero_bytes"].as_u64().unwrap() > 0);
}
