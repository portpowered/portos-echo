//! T3 integration: prebuilt binaries, pinned host, local fake APIs; no Cargo in tests.
#[cfg(test)]
mod sim_support;
use serde_json::{Value, json};
use sim_support::{Session, records};

#[test]
#[ignore = "T3 requires the prebuilt pinned hassmic-host"]
fn native_mute_roundtrip_uses_the_sim_control_path_and_next_wake_streams() {
    let session = Session::new();
    session.scenario(
        json!([
            {"press":"mute"}, {"press":{"key":"mute"}},
            {"say":"fixture.pcm"},
            {"expect_trace":["wyoming.run_started","wyoming.mic_frames {count_gte: 10}","wyoming.run_ended"]}
        ]),
        json!({}),
    );
    let output = session
        .command(env!("CARGO_BIN_EXE_echo-lab-sim-bridge"))
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
    assert_eq!(report["observations"], observed_frames(&session).unwrap());
    let observations = report["observations"].as_array().unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0]["run_id"], "run-1");
    assert!(observations[0]["count"].as_u64().unwrap() >= 10);
    session.endpoints();
}

#[test]
#[ignore = "T3 requires the prebuilt pinned hassmic-host"]
fn w1_fixture_pass_seeded_failure_and_action_use_the_same_runner() {
    for (steps, expected) in [
        (
            json!([{"say":"fixture.pcm"},{"expect_trace":["wyoming.run_started","wyoming.mic_frames {count_gte: 10}","wyoming.run_ended"]}]),
            0,
        ),
        (
            json!([{"say":"fixture.pcm"},{"expect_trace":["wyoming.never_happened"]},{"press":"action"}]),
            1,
        ),
        (
            json!([{"press":"action"},{"expect_trace":["wyoming.run_started","wyoming.mic_frames {count_gte: 10}","wyoming.run_ended"]}]),
            0,
        ),
    ] {
        let session = Session::new();
        session.scenario(steps, json!({}));
        let output = session
            .command(env!("CARGO_BIN_EXE_echo-lab-sim-bridge"))
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = session.report();
        assert_eq!(report["observations"], observed_frames(&session).unwrap());
        assert_eq!(report["result"], if expected == 0 { "pass" } else { "fail" });
        assert_eq!(report["checks"]["step_1"], expected == 0);
        assert_eq!(report["checks"]["world_closed"], true);
        if expected == 1 {
            assert!(!report["checks"].as_object().unwrap().contains_key("step_2"));
            assert!(String::from_utf8_lossy(&output.stderr).contains("wyoming.never_happened"));
        }
    }
}

#[test]
#[ignore = "T3 requires the prebuilt pinned hassmic-host"]
fn scripted_observers_are_strict_and_release_all_three_loopback_endpoints() {
    let session = Session::new();
    session.scenario(
        json!([{"expect_trace":["wyoming.run_started","wyoming.mic_frames {count_gte: 10}","wyoming.run_ended"]},
        {"expect_screen":{"state":"speaking"}}, {"expect_speech_contains":["test","echo","[a.*]"]}]),
        json!({"first_audio_after_endpoint_ms_p95":1500}),
    );
    let output = session.script(&json!({"records":records(),"ui":{"state":"speaking"}}));
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        session.report()["checks"],
        json!({"step_0":true,"step_1":true,"step_2":true,"step_3":true,"world_closed":true})
    );
    session.endpoints();
    let events = std::fs::read_to_string(session.0.join("artifacts/events.jsonl")).unwrap();
    assert!(!events.contains("TEST echo"));
    assert!(!events.contains("[a.*]"));
}

#[test]
#[ignore = "T3 requires the prebuilt pinned hassmic-host"]
fn late_duplicates_and_incomplete_traces_fail_at_finalization() {
    for tail in [
        json!({"late":[{"event":"wyoming.mic_frames","elapsed_ms":5,"run_id":"one","count":10}]}),
        json!({"raw_tail":"{\"event\":"}),
        json!({"raw_tail":"malformed\n"}),
    ] {
        let session = Session::new();
        session.scenario(
            json!([{"expect_trace":["wyoming.run_started","wyoming.mic_frames {count_gte: 10}","wyoming.run_ended"]}]),
            json!({}),
        );
        let mut script = json!({"records":records()});
        script
            .as_object_mut()
            .unwrap()
            .extend(tail.as_object().unwrap().clone());
        assert_eq!(session.script(&script).status.code(), Some(1));
        let report = session.report();
        assert_eq!(report["result"], "fail");
        assert_eq!(report["checks"]["step_0"], false);
        if tail.get("late").is_some() {
            assert_eq!(report["observations"], observed_frames(&session).unwrap());
            assert_eq!(report["observations"].as_array().unwrap().len(), 2);
        } else {
            assert_eq!(report["observations"], json!([]));
        }
        session.endpoints();
    }
}

#[test]
#[ignore = "T3 requires the prebuilt pinned hassmic-host"]
fn cancellation_and_combined_primary_cleanup_failure_never_pass_remaining_steps() {
    let session = Session::new();
    session.scenario(
        json!([{"expect_trace":["wyoming.absent"]},{"press":"action"}]),
        json!({}),
    );
    let output = session.script(&json!({"records":records(),"fail_close":true}));
    assert_eq!(output.status.code(), Some(1));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("wyoming.absent") && error.contains("cleanup: bridge failed to close"),
        "{error}"
    );
    assert_eq!(session.report()["checks"], json!({"step_0":false,"world_closed":false}));
    assert_eq!(session.report()["observations"], observed_frames(&session).unwrap());
    session.endpoints();
    let cancel = Session::new();
    cancel.scenario(json!([{"say":"fixture.pcm"},{"press":"action"}]), json!({}));
    std::fs::write(
        cancel.0.join("script.json"),
        json!({"records":records(),"cancel":true}).to_string(),
    )
    .unwrap();
    let output = cancel
        .command(env!("CARGO_BIN_EXE_echo-lab-sim-script"))
        .args(["--cancel-file", "cancel"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cancelled"),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(cancel.report()["checks"], json!({"step_0":false,"world_closed":true}));
    assert!(cancel.0.join("cancel").exists());
    cancel.endpoints();
}

fn observed_frames(session: &Session) -> Result<Value, Box<dyn std::error::Error>> {
    let events = std::fs::read_to_string(session.0.join("artifacts/events.jsonl"))?;
    let records = events
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!(
        records
            .into_iter()
            .filter(|record| record["event"] == "wyoming.mic_frames")
            .map(|record| json!({"event":record["event"],"run_id":record["run_id"],"count":record["count"]}))
            .collect::<Vec<_>>()
    ))
}

#[test]
#[ignore = "T3 requires the prebuilt pinned hassmic-host"]
fn actual_counts_survive_impossible_threshold_and_keep_runs_distinct() {
    for (threshold, runs, expected, result) in [(1_000_000, 1, 1, "fail"), (10, 2, 0, "pass")] {
        let session = Session::new();
        let mut steps = Vec::new();
        for _ in 0..runs {
            steps.push(json!({"say":"fixture.pcm"}));
            steps.push(json!({"expect_trace":["wyoming.run_started",
                format!("wyoming.mic_frames {{count_gte: {threshold}}}"),"wyoming.run_ended"]}));
        }
        steps.push(json!({"press":"pair"}));
        session.scenario(json!(steps), json!({}));
        let output = session
            .command(env!("CARGO_BIN_EXE_echo-lab-sim-bridge"))
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = session.report();
        assert_eq!(report["result"], result);
        assert_eq!(report["checks"]["world_closed"], true);
        assert_eq!(report["observations"], observed_frames(&session).unwrap());
        let observations = report["observations"].as_array().unwrap();
        assert_eq!(observations.len(), runs);
        for observation in observations {
            let count = observation["count"].as_u64().unwrap();
            assert!((10..1_000_000).contains(&count));
        }
        if runs == 2 {
            assert_ne!(observations[0]["run_id"], observations[1]["run_id"]);
        } else {
            assert_eq!(report["checks"]["step_1"], false);
            assert!(report["checks"].get("step_2").is_none());
        }
        assert_eq!(report["evidence"], json!(["events.jsonl"]));
    }
}

#[test]
#[ignore = "T3 requires the prebuilt pinned hassmic-host"]
fn missing_and_invalid_counts_never_publish_invented_observations() {
    for count in [None, Some(json!(-1))] {
        let session = Session::new();
        session.scenario(
            json!([{"expect_trace":["wyoming.mic_frames {count_gte: 10}"]}]),
            json!({}),
        );
        let mut trace = records();
        if let Some(count) = count {
            trace[1]["count"] = count;
        } else {
            trace[1].as_object_mut().unwrap().remove("count");
        }
        assert_eq!(session.script(&json!({"records":trace})).status.code(), Some(1));
        let report = session.report();
        assert_eq!(report["result"], "fail");
        assert_eq!(report["checks"]["step_0"], false);
        assert_eq!(report["observations"], json!([]));
    }
}

#[test]
#[ignore = "T3 requires the prebuilt pinned hassmic-host"]
fn absent_microphone_records_and_trace_files_have_no_observations() {
    for script in [
        json!({"records":[],"missing_trace":true}),
        json!({"records":[{"event":"wyoming.run_started","elapsed_ms":0,"run_id":"one"},
        {"event":"wyoming.run_ended","elapsed_ms":1,"run_id":"one"}]}),
    ] {
        let session = Session::new();
        session.scenario(
            json!([{"expect_trace":["wyoming.mic_frames {count_gte: 10}"]}]),
            json!({}),
        );
        assert_eq!(session.script(&script).status.code(), Some(1));
        assert_eq!(session.report()["result"], "fail");
        assert_eq!(session.report()["observations"], json!([]));
    }
}

#[test]
#[ignore = "T3 requires the prebuilt pinned hassmic-host"]
fn observation_projection_excludes_private_payloads_and_preserves_explicit_zero() {
    let session = Session::new();
    session.scenario(
        json!([{"expect_trace":["wyoming.mic_frames {count_gte: 0}"]}]),
        json!({}),
    );
    let mut trace = records();
    trace[1]["count"] = json!(0);
    for key in ["payload", "credential", "provider"] {
        trace[1][key] = json!("synthetic-private-marker");
    }
    assert_eq!(
        session
            .script(&json!({"records":trace,"ui":{"state":"speaking"}}))
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        session.report()["observations"],
        json!([{"event":"wyoming.mic_frames","run_id":"one","count":0}])
    );
    for file in ["report.json", "events.jsonl"] {
        let text = std::fs::read_to_string(session.0.join("artifacts").join(file)).unwrap();
        for forbidden in ["synthetic-private-marker", "TEST echo", "speaking"] {
            assert!(!text.contains(forbidden));
        }
    }
}

// T3: customer CLI, prebuilt production bridge, pinned host and owned local peers.
#[test]
#[ignore = "T3 requires the prebuilt pinned hassmic-host and echo-bridge"]
fn m4_cli_both_cases_and_wrong_count_report_exact_results_and_cleanup() {
    let bridge = std::path::Path::new(env!("CARGO_BIN_EXE_echo-lab")).with_file_name(if cfg!(windows) {
        "echo-bridge.exe"
    } else {
        "echo-bridge"
    });
    for seeded in [false, true] {
        let session = Session::new();
        let original = include_str!("../../../tests/scenarios/m4-spoof-tool.yaml");
        let scenario = if seeded {
            original.replace("count: 1", "count: 2")
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
        let (result, count) = if seeded { ("fail", 1) } else { ("pass", 2) };
        assert_eq!(report["result"], result);
        assert_eq!(report["checks"]["world_closed"], true);
        assert_eq!(report["checks"]["step_1"], !seeded);
        let observations = report["observations"].as_array().unwrap();
        assert_eq!(observations.len(), count);
        for (index, observation) in observations.iter().enumerate() {
            assert_m4_case(observation, index == 1);
        }
        assert_m4_seed(&output, seeded);
        assert!(!session.0.join("artifacts/private-success").exists());
        assert!(!session.0.join("artifacts/private-http500").exists());
    }
}

fn assert_m4_case(observation: &Value, negative: bool) {
    assert_eq!(observation["case"], if negative { "http500" } else { "success" });
    assert_eq!(observation["tools_call_count"], 1);
    assert_eq!(observation["commentary_count"], 1);
    assert_eq!(observation["capture_bytes"], 28800);
    assert_eq!(observation["reply_pcm_bytes"], 9600);
    assert_eq!(observation["screen"], "speaking");
    assert_eq!(
        observation["transcript"],
        if negative {
            "I couldn't call test echo."
        } else {
            "test echo: delegated echo"
        }
    );
    assert!(
        observation["endpoint_to_first_pcm_us"]
            .as_u64()
            .is_some_and(|micros| micros <= 1_500_000)
    );
}

fn assert_m4_seed(output: &std::process::Output, seeded: bool) {
    if seeded {
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("step 1:"));
        assert!(error.contains("expected 2 occurrences, observed 1"));
    }
}
