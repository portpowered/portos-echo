//! T3: actual CLI, production bridge, native host, isolated local provider.
use super::*;

fn scenario(session: &Session, contents: &str) {
    fs::write(session.0.join("scenario.yaml"), contents).unwrap();
}
const SCENARIO: &str = include_str!("../../../../../tests/scenarios/m3-full-duplex.yaml");

fn command(session: &Session) -> Command {
    let mut command = session.cli();
    command
        .current_dir(&session.0)
        .args(["scenario.yaml", "--driver", "sim", "--out", "artifacts"]);
    command
}

fn checked(session: &Session, output: &Output, result: &str) -> Value {
    assert_eq!(
        output.status.code(),
        Some(i32::from(result != "pass")),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = session.report();
    assert_eq!(report["result"], result);
    assert_eq!(report["checks"]["world_closed"], true);
    session.owners_closed();
    // The collected CLI reports world_closed only after World::close joins
    // its relay/Live tasks and collects the bridge and native host. Private
    // lifecycle tests protect that per-owner boundary, including failed
    // collection. Reconnecting here would query a reusable address instead.
    assert!(!session.0.join("artifacts/m3.private").exists());
    let events = fs::read_to_string(session.0.join("artifacts/events.jsonl")).unwrap();
    assert!(!events.contains("synthetic-local-api"));
    assert!(!events.contains("synthetic-local-screen"));
    let endpoints: Vec<std::net::SocketAddr> = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|record| record["event"] == "sim.endpoint")
        .map(|record| record["address"].as_str().unwrap().parse().unwrap())
        .collect();
    assert_eq!(endpoints.len(), 5);
    for address in &endpoints[1..] {
        assert_eq!(address.ip(), std::net::Ipv4Addr::LOCALHOST);
    }
    report
}

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host"]
fn m3_parallel_worlds_prove_overlap_cut_second_answer_and_native_mute() {
    let first = Session::new();
    let second = Session::new();
    for session in [&first, &second] {
        scenario(session, SCENARIO);
    }
    let a = command(&first)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let b = command(&second)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    for (session, output) in [
        (&first, a.wait_with_output().unwrap()),
        (&second, b.wait_with_output().unwrap()),
    ] {
        let report = checked(session, &output, "pass");
        assert_eq!(report["checks"]["m3_duplex"], true);
        assert_eq!(report["checks"]["m3_mute"], true);
        assert_eq!(report["observations"].as_array().unwrap().len(), 1);
        let events = fs::read_to_string(session.0.join("artifacts/events.jsonl")).unwrap();
        for phrase in [
            "runs=1 inputs=2",
            "first_reply_played=true",
            "barge_in_budget_ms=600",
            "reply_send_window_ms=200",
            "reply_bytes_after_cancel_window=0",
            "second_answer_played=true",
            "muted_outbound_frames=0",
            "live_received_frame_delta=0",
        ] {
            assert!(events.contains(phrase), "{phrase}");
        }
    }
}

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host"]
fn m3_missing_playback_expectation_fails_and_closes_private_world() {
    let session = Session::new();
    scenario(&session, &SCENARIO.replace("outcome: played", "outcome: absent"));
    let output = command(&session).output().unwrap();
    checked(&session, &output, "fail");
    assert!(String::from_utf8_lossy(&output.stderr).contains("expected 1 occurrences, observed 0"));
}

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host"]
fn m3_caller_cancels_overlap_and_collects_children_without_inventing_answer() {
    let session = Session::new();
    scenario(&session, SCENARIO);
    let child = command(&session)
        .args(["--cancel-file", "cancel"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if fs::read_to_string(session.0.join("artifacts/events.jsonl")).is_ok_and(|text| {
            text.lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .any(|v| v["event"] == "step_started" && v["step"] == 3)
        }) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "overlap never started");
        std::thread::yield_now();
    }
    fs::write(session.0.join("cancel"), "cancel").unwrap();
    let output = child.wait_with_output().unwrap();
    checked(&session, &output, "fail");
    assert!(String::from_utf8_lossy(&output.stderr).contains("cancelled"));
}

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host"]
fn m3_check_and_normal_discovery_execute_the_same_production_scenario() {
    let session = Session::new();
    let root = session.0.join("tests/scenarios");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("m3-full-duplex.yaml"), SCENARIO).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_echo-lab"))
        .current_dir(&session.0)
        .env("TMP", &session.0)
        .env("TEMP", &session.0)
        .env("TMPDIR", &session.0)
        .args(["check", "--milestone", "M3"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(env!("CARGO_BIN_EXE_echo-lab"))
        .current_dir(&session.0)
        .args(["run", "--all", "--driver", "sim", "--out", "suite"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).ends_with("executed=1 passed=1 failed=0 not_runnable=0\n"));
}
