//! T3 integration: prebuilt binaries, pinned host, local fake APIs; no Cargo in tests.
#[cfg(test)]
mod sim_support;
use serde_json::json;
use sim_support::{Session, records};

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
        assert_eq!(session.report()["result"], "fail");
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
    assert!(String::from_utf8_lossy(&output.stderr).contains("cancelled"));
    assert_eq!(cancel.report()["checks"], json!({"step_0":false,"world_closed":true}));
    assert!(cancel.0.join("cancel").exists());
    cancel.endpoints();
}
