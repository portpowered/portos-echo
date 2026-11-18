//! T3: the delivered production child, real authenticated SSE and pinned host.
use super::*;

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host"]
fn m1_prefix_selects_production_and_observes_ready_before_a_complete_wake() {
    let session = Session::new();
    session.scenario(
        json!([
            {"expect_screen":{"state":"ready","message":"Echo Live ready"}},
            {"say":"fixture.pcm"},
            {"expect_trace":["wyoming.run_started","wyoming.mic_frames {count_gte: 10}","wyoming.run_ended"]}
        ]),
        json!({}),
    );
    let path = session.0.join("scenario.yaml");
    let mut scenario: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    scenario["milestone"] = json!("M1");
    fs::write(path, scenario.to_string()).unwrap();
    let output = session
        .cli()
        .current_dir(&session.0)
        .args(["scenario.yaml", "--driver", "sim", "--out", "artifacts"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = session.report();
    assert_eq!(
        report["checks"],
        json!({"step_0":true,"step_1":true,"step_2":true,"world_closed":true})
    );
    let observations = report["observations"].as_array().unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0]["run_id"], "run-1");
    assert!(observations[0]["count"].as_u64().unwrap() >= 10);
    let events = fs::read_to_string(session.0.join("artifacts/events.jsonl")).unwrap();
    let addresses: Vec<std::net::SocketAddr> = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|record| record["event"] == "sim.endpoint")
        .map(|record| record["address"].as_str().unwrap().parse().unwrap())
        .collect();
    assert_eq!(addresses.len(), 6);
    session.owners_closed();
    assert!(!session.0.join("artifacts/configured.private").exists());
    assert!(!events.contains("synthetic-local-api"));
    assert!(!events.contains("synthetic-local-screen"));
}

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host"]
fn m1_native_muted_say_measures_zero_and_unmuted_next_wake_streams() {
    let session = Session::new();
    session.scenario(
        json!([
            {"expect_screen":{"state":"ready","message":"Echo Live ready"}},
            {"say":"fixture.pcm"},
            {"expect_trace":["wyoming.run_started","wyoming.mic_frames {count_gte: 10}","wyoming.run_ended"]},
            {"press":"mute"}, {"expect_screen":{"state":"muted"}},
            {"say":"fixture.pcm"}, {"press":"mute"},
            {"expect_screen":{"state":"ready","message":"Echo Live ready"}},
            {"expect_trace":["mic.mute_window {window_id: mute-1, received_frames: 0, forwarded_frames: 0}"]},
            {"say":"fixture.pcm"},
            {"expect_trace":["wyoming.run_started","wyoming.mic_frames {count_gte: 10}","wyoming.run_ended"]}
        ]),
        json!({}),
    );
    let output = session
        .command(
            std::path::Path::new(env!("CARGO_BIN_EXE_echo-lab"))
                .parent()
                .unwrap()
                .join("echo-bridge")
                .to_str()
                .unwrap(),
        )
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
    assert_eq!(report["checks"].as_object().unwrap().len(), 12);
    assert!(report["checks"].as_object().unwrap().values().all(|v| v == true));
    let observations = report["observations"].as_array().unwrap();
    assert_eq!(observations.len(), 2);
    for (index, observation) in observations.iter().enumerate() {
        assert_eq!(observation["run_id"], format!("run-{}", index + 1));
        assert!(observation["count"].as_u64().unwrap() >= 10);
    }
    assert!(!session.0.join("artifacts/configured.private").exists());
}

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host"]
fn m1_independent_restart_steps_preserve_prior_generation_oracles() {
    let session = Session::new();
    session.scenario(
        json!([
            {"expect_screen":{"state":"ready","message":"Echo Live ready"}},
            {"say":"fixture.pcm"},
            {"expect_trace":["wyoming.run_started","wyoming.mic_frames {count_gte: 10}","wyoming.run_ended"]},
            {"restart":{"process":"bridge","count":1,"say":"fixture.pcm"}},
            {"restart":{"process":"hassmic-host","count":1,"say":"fixture.pcm"}}
        ]),
        json!({}),
    );
    let bridge = std::path::Path::new(env!("CARGO_BIN_EXE_echo-lab")).with_file_name("echo-bridge");
    let output = session.command(bridge.to_str().unwrap()).output().unwrap();
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
        json!({"step_0":true,"step_1":true,"step_2":true,"step_3":true,"step_4":true,"world_closed":true})
    );
    let observations = report["observations"].as_array().unwrap();
    assert_eq!(observations.len(), 3);
    assert_eq!(
        observations
            .iter()
            .map(|r| r["run_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["run-1", "run-1", "run-2"]
    );
    assert!(observations.iter().all(|r| r["count"].as_u64().unwrap() >= 10));
}
