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
    assert_eq!(addresses.len(), 5);
    for address in &addresses[1..] {
        assert!(
            std::net::TcpStream::connect(address).is_err(),
            "listener leaked: {address}"
        );
    }
    assert!(!session.0.join("artifacts/configured.private").exists());
    assert!(!events.contains("synthetic-local-api"));
    assert!(!events.contains("synthetic-local-screen"));
}
