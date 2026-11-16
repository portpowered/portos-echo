//! T3 M5 runner witnesses consume prebuilt production artifacts.
use super::Session;
use serde_json::{Value, json};

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host"]
fn m5_authored_scenario_measures_all_eight_outcomes_and_isolates_a_failed_count_oracle() {
    // Run independent sessions in parallel. The seeded failed oracle must not
    // contaminate the healthy grant, screen, endpoints or report.
    std::thread::scope(|scope| {
        let healthy = scope.spawn(|| authored(false));
        let seeded = scope.spawn(|| authored(true));
        healthy.join().unwrap();
        seeded.join().unwrap();
    });
}

fn authored(seed: bool) {
    let session = Session::new();
    let mut scenario: Value =
        serde_norway::from_str(include_str!("../../../../tests/scenarios/m5-port-os-link.yaml")).unwrap();
    let last = scenario["steps"].as_array().unwrap().len() - 1;
    let mut actual = scenario["steps"][last]["expect_link_counts"].clone();
    actual["tool_call"] = json!(1);
    if seed {
        scenario["steps"][last]["expect_link_counts"]["tool_call"] = json!(2);
    }
    std::fs::write(session.0.join("scenario.yaml"), scenario.to_string()).unwrap();
    let output = run(&session);
    assert_eq!(
        output.status.code(),
        Some(i32::from(seed)),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = session.report();
    assert_eq!(report["result"], if seed { "fail" } else { "pass" });
    assert_eq!(report["link_counts"], actual);
    assert_eq!(report["checks"]["world_closed"], true);
    assert_eq!(report["checks"][format!("step_{last}")], !seed);
    if seed {
        assert!(String::from_utf8_lossy(&output.stderr).contains("link counts: expected"));
    } else {
        assert_eq!(output.stderr, [] as [u8; 0]);
    }
    assert!(!session.0.join("artifacts/mcp.private").exists());
    authored_cleanup(&session, &report, &output);
}

fn authored_cleanup(session: &Session, report: &Value, output: &std::process::Output) {
    let events = std::fs::read_to_string(session.0.join("artifacts/events.jsonl")).unwrap();
    for private in [
        "synthetic-refresh-",
        "synthetic-api-key",
        "private_key_jwk",
        "synthetic-m5-screen",
    ] {
        assert!(!events.contains(private));
        assert!(!report.to_string().contains(private));
        assert!(!String::from_utf8_lossy(&output.stdout).contains(private));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(private));
    }
    let endpoints = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["event"] == "sim.endpoint")
        .collect::<Vec<_>>();
    assert_eq!(endpoints.len(), 10);
    for endpoint in &endpoints[1..] {
        assert!(std::net::TcpStream::connect(endpoint["address"].as_str().unwrap()).is_err());
    }
}

#[test]
fn m5_restart_without_a_running_bridge_fails_before_launch() {
    let session = Session::new();
    scenario(&session, json!([{"mcp":{"action":"restart"}}]));
    let output = session
        .command(env!("CARGO_BIN_EXE_echo-lab-sim-script"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "echo-lab: scenario.yaml: step 0: M5: restart requires a running linked bridge\n"
    );
    let report = session.report();
    assert_eq!(report["checks"], json!({"step_0":false,"world_closed":true}));
    assert!(report.get("link_counts").is_none());
    assert!(!session.0.join("artifacts/mcp.private").exists());
    assert!(
        !std::fs::read_to_string(session.0.join("artifacts/events.jsonl"))
            .unwrap()
            .contains("sim.endpoint")
    );
}

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host"]
fn m5_restart_never_cancels_a_pending_link_or_resurrects_an_unlinked_grant() {
    for (action, message, relink, unlink) in [
        ("link", "M5: restart refused while link is pending", 0, 0),
        ("unlink", "M5: restart requires a stored grant", 1, 1),
    ] {
        let session = Session::new();
        scenario(
            &session,
            json!([{"mcp":{"action":action}}, {"mcp":{"action":"restart"}}]),
        );
        let output = run(&session);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!("echo-lab: scenario.yaml: step 1: {message}\n")
        );
        let report = session.report();
        assert_eq!(report["result"], "fail");
        assert_eq!(
            report["checks"],
            json!({"step_0":true,"step_1":false,"world_closed":true})
        );
        assert_eq!(
            report["link_counts"],
            json!({"link":0,"tools_list":0,"tool_call":0,"forced_refresh":0,
            "replay_rejected":0,"relink_error":relink,"restart_grant_kept":0,"unlink":unlink})
        );
        assert!(!session.0.join("artifacts/mcp.private").exists());
    }
}

fn scenario(session: &Session, steps: Value) {
    session.scenario(steps, json!({}));
    let path = session.0.join("scenario.yaml");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["milestone"] = json!("M5");
    std::fs::write(path, value.to_string()).unwrap();
}

fn run(session: &Session) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_echo-lab"))
        .current_dir(&session.0)
        .args(["run", "scenario.yaml", "--driver", "sim", "--out", "artifacts"])
        .output()
        .unwrap()
}

#[test]
fn m5_resource_controls_require_a_link_before_host_or_bridge_launch() {
    for action in ["call", "probe"] {
        let session = Session::new();
        session.scenario(json!([{"mcp":{"action":action}}]), json!({}));
        let path = session.0.join("scenario.yaml");
        let mut scenario: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        scenario["milestone"] = json!("M5");
        std::fs::write(path, scenario.to_string()).unwrap();
        let output = session
            .command(env!("CARGO_BIN_EXE_echo-lab-sim-script"))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "echo-lab: scenario.yaml: step 0: M5: resource control requires a running linked bridge\n"
        );
        let report = session.report();
        assert_eq!(report["result"], "fail");
        assert_eq!(report["checks"], json!({"step_0":false,"world_closed":true}));
        assert!(report.get("link_counts").is_none());
        let events = std::fs::read_to_string(session.0.join("artifacts/events.jsonl")).unwrap();
        assert!(!events.contains("sim.endpoint"));
        assert!(!session.0.join("artifacts/mcp.private").exists());
    }
}

#[test]
fn m5_replay_without_a_running_rotated_grant_fails_before_launch() {
    let session = Session::new();
    scenario(&session, json!([{"mcp":{"action":"replay"}}]));
    let output = session
        .command(env!("CARGO_BIN_EXE_echo-lab-sim-script"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "echo-lab: scenario.yaml: step 0: M5: replay requires a live rotated grant\n"
    );
    let report = session.report();
    assert_eq!(report["checks"], json!({"step_0":false,"world_closed":true}));
    assert!(report.get("link_counts").is_none());
    assert!(!session.0.join("artifacts/mcp.private").exists());
    assert!(
        !std::fs::read_to_string(session.0.join("artifacts/events.jsonl"))
            .unwrap()
            .contains("sim.endpoint")
    );
}

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host"]
fn m5_replay_revokes_the_rotated_grant_after_restart_and_retains_exact_counts() {
    let session = Session::new();
    scenario(
        &session,
        json!([
            {"mcp":{"action":"link"}}, {"mcp":{"action":"approve"}},
            {"mcp":{"action":"refresh"}}, {"mcp":{"action":"restart"}},
            {"mcp":{"action":"refresh"}}, {"mcp":{"action":"replay"}},
            {"mcp":{"action":"unlink"}},
            {"expect_screen":{"state":"error","relink":true,
                "message":"Run echo-bridge link with your config to relink Port OS MCP."}},
            {"expect_link_counts":{"link":1,"tools_list":0,"tool_call":0,"forced_refresh":2,
                "replay_rejected":1,"relink_error":1,"restart_grant_kept":0,"unlink":1}}
        ]),
    );
    let output = run(&session);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = session.report();
    assert_eq!(report["result"], "pass");
    assert_eq!(
        report["link_counts"],
        json!({"link":1,"tools_list":0,"tool_call":0,"forced_refresh":2,
        "replay_rejected":1,"relink_error":1,"restart_grant_kept":0,"unlink":1})
    );
    assert_eq!(report["checks"]["world_closed"], true);
    assert!(!session.0.join("artifacts/mcp.private").exists());
    let events = std::fs::read_to_string(session.0.join("artifacts/events.jsonl")).unwrap();
    for private in [
        "synthetic-refresh-",
        "synthetic-api-key",
        "private_key_jwk",
        "m5-external-replay",
    ] {
        assert!(!events.contains(private));
        assert!(!String::from_utf8_lossy(&output.stdout).contains(private));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(private));
        assert!(!report.to_string().contains(private));
    }
}

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host"]
fn m5_replay_before_rotation_never_consumes_the_live_refresh_token() {
    let session = Session::new();
    scenario(
        &session,
        json!([
            {"mcp":{"action":"link"}}, {"mcp":{"action":"approve"}}, {"mcp":{"action":"replay"}}
        ]),
    );
    let output = run(&session);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "echo-lab: scenario.yaml: step 2: M5: replay requires a live rotated grant\n"
    );
    let report = session.report();
    assert_eq!(report["result"], "fail");
    assert_eq!(
        report["checks"],
        json!({"step_0":true,"step_1":true,"step_2":false,"world_closed":true})
    );
    assert_eq!(
        report["link_counts"],
        json!({"link":1,"tools_list":0,"tool_call":0,"forced_refresh":0,
        "replay_rejected":0,"relink_error":0,"restart_grant_kept":0,"unlink":0})
    );
    assert!(!session.0.join("artifacts/mcp.private").exists());
}

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host"]
fn m5_replay_cannot_repeat_a_rejected_refresh() {
    let session = Session::new();
    scenario(
        &session,
        json!([
            {"mcp":{"action":"link"}}, {"mcp":{"action":"approve"}},
            {"mcp":{"action":"refresh"}}, {"mcp":{"action":"replay"}}, {"mcp":{"action":"replay"}}
        ]),
    );
    let output = run(&session);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "echo-lab: scenario.yaml: step 4: M5: replay requires a live rotated grant\n"
    );
    let report = session.report();
    assert_eq!(report["result"], "fail");
    assert_eq!(
        report["checks"],
        json!({"step_0":true,"step_1":true,"step_2":true,
        "step_3":true,"step_4":false,"world_closed":true})
    );
    assert_eq!(
        report["link_counts"],
        json!({"link":1,"tools_list":0,"tool_call":0,"forced_refresh":1,
        "replay_rejected":1,"relink_error":0,"restart_grant_kept":0,"unlink":0})
    );
    assert!(!session.0.join("artifacts/mcp.private").exists());
}

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host"]
fn m5_pending_link_is_not_approval_and_failed_count_oracle_closes_the_world() {
    let session = Session::new();
    let expected = json!({"link":1,"tools_list":0,"tool_call":0,"forced_refresh":0,
        "replay_rejected":0,"relink_error":0,"restart_grant_kept":0,"unlink":0});
    session.scenario(
        json!([{"mcp":{"action":"link"}}, {"expect_link_counts":expected}]),
        json!({}),
    );
    let path = session.0.join("scenario.yaml");
    let mut scenario: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    scenario["milestone"] = json!("M5");
    std::fs::write(path, scenario.to_string()).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_echo-lab"))
        .current_dir(&session.0)
        .args(["run", "scenario.yaml", "--driver", "sim", "--out", "artifacts"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report = session.report();
    assert_eq!(report["result"], "fail");
    assert_eq!(
        report["checks"],
        json!({"step_0":true,"step_1":false,"world_closed":true})
    );
    let mut actual = expected.clone();
    actual["link"] = json!(0);
    assert_eq!(report["link_counts"], actual);
    let error = String::from_utf8_lossy(&output.stderr);
    let diagnostic = error
        .strip_prefix("echo-lab: scenario.yaml: step 1: link counts: expected ")
        .unwrap();
    let (reported_expected, reported_actual) = diagnostic.trim_end().split_once("; actual ").unwrap();
    assert_eq!(serde_json::from_str::<Value>(reported_expected).unwrap(), expected);
    assert_eq!(serde_json::from_str::<Value>(reported_actual).unwrap(), actual);
    assert!(!session.0.join("artifacts/mcp.private").exists());
    let events = std::fs::read_to_string(session.0.join("artifacts/events.jsonl")).unwrap();
    let endpoints = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["event"] == "sim.endpoint")
        .collect::<Vec<_>>();
    assert_eq!(endpoints.len(), 7);
    for endpoint in &endpoints[1..] {
        assert!(std::net::TcpStream::connect(endpoint["address"].as_str().unwrap()).is_err());
    }
}

// Partial M5 journey: exact CLI/screen/grant observations, not the final M5
// tool/replay scenario; a restart without resource use must not count as reuse. The shell owns both prebuilt deliverables.
#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host"]
fn m5_operator_controls_use_production_bridge_and_report_observed_partial_counts() {
    let session = Session::new();
    session.scenario(json!([
        {"mcp":{"action":"link"}},
        {"expect_screen":{"state":"link_code","user_code":"TEST-CODE","expires_at_unix":1_800_001_800_u64}},
        {"mcp":{"action":"approve"}},
        {"expect_screen":{"state":"ready","message":"Echo Live ready"}},
        {"mcp":{"action":"refresh"}}, {"mcp":{"action":"restart"}},
        {"expect_screen":{"state":"ready","message":"Echo Live ready"}}, {"mcp":{"action":"refresh"}},
        {"mcp":{"action":"unlink"}},
        {"expect_screen":{"state":"error","relink":true,"message":"Run echo-bridge link with your config to relink Port OS MCP."}},
        {"expect_trace":["mcp.link.approved", "mcp.refresh.committed {count: 2}", "mcp.unlink.completed"]},
        {"expect_link_counts":{"link":1,"tools_list":0,"tool_call":0,"forced_refresh":2,
            "replay_rejected":0,"relink_error":1,"restart_grant_kept":0,"unlink":1}}
    ]), json!({}));
    let path = session.0.join("scenario.yaml");
    let mut scenario: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    scenario["milestone"] = json!("M5");
    std::fs::write(path, scenario.to_string()).unwrap();
    // Omit --bridge deliberately: M5 defaults to the actual delivered binary.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_echo-lab"))
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
    assert_eq!(report["link_counts"], scenario["steps"][11]["expect_link_counts"]);
    assert_eq!(report["checks"]["world_closed"], true);
    assert_eq!(report["observations"], json!([]));
    assert!(!session.0.join("artifacts/mcp.private").exists());
    let events = std::fs::read_to_string(session.0.join("artifacts/events.jsonl")).unwrap();
    for private in [
        "synthetic-refresh-",
        "synthetic-api-key",
        "synthetic-m5-screen",
        "private_key_jwk",
    ] {
        assert!(!events.contains(private));
    }
    let endpoints = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["event"] == "sim.endpoint")
        .collect::<Vec<_>>();
    assert_eq!(endpoints.len(), 10);
    for endpoint in &endpoints[1..] {
        assert!(std::net::TcpStream::connect(endpoint["address"].as_str().unwrap()).is_err());
    }
}
