//! T3 lifecycle witnesses through the delivered CLI and controlled child processes.
use crate::run_support::{Session, failed};
use serde_json::{Value, json};
use std::fs;

fn events(session: &Session) -> Vec<String> {
    fs::read_to_string(session.0.join("artifacts/events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["event"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

fn holds() -> Value {
    json!([
        {"holdId":"fixture-first", "lease":"fixture-first-lease", "listening":true,"talking":true},
        {"holdId":"fixture-second", "lease":"fixture-second-lease", "listening":true,"talking":true}
    ])
}

#[test]
fn f08_rotate_before_ceiling_wait_exactly_25_seconds_without_chirp() {
    let session = Session::new(&json!({"times":[0,0,600_000,625_000,625_000],"holds":holds()}));
    session.scenario("two.yaml", &json!([{"say":"utterance.wav"}]));
    let output = session.run(&["two.yaml"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        session.operations(),
        [
            "digest",
            "firmware",
            "firmware",
            "firmware",
            "clock",
            "hold",
            "clock",
            "play",
            "reply",
            "firmware",
            "firmware",
            "firmware",
            "clock",
            "wait_until",
            "release",
            "wait",
            "clock",
            "hold",
            "clock",
            "play",
            "reply",
            "wait_until",
            "release"
        ]
    );
    let ledger = session.ledger();
    assert_eq!(
        ledger[14]["args"],
        json!(["session", "release", "fixture-first", "--json"])
    );
    assert_eq!(ledger[15]["args"], json!(["-c", "<host-helper>", "wait", "25000"]));
    assert_eq!(ledger[19]["args"][4], "fixture-second-lease");
    assert_eq!(
        session.report()["bench_session_ids"],
        json!(["batch-hold-1", "batch-hold-2"])
    );
    assert_rotation_events(&session);
}

fn assert_rotation_events(session: &Session) {
    assert_eq!(
        events(session),
        [
            "run_started",
            "step_started",
            "hold_opened",
            "step_completed",
            "step_started",
            "hold_closed",
            "cooldown_started",
            "cooldown_completed",
            "hold_opened",
            "step_completed",
            "hold_closed",
            "run_completed"
        ]
    );
}

#[test]
fn f09_confirmed_preemption_retries_only_the_interrupted_step_with_new_lease() {
    let session = Session::new(&json!({"preemptions":1,"holds":holds()}));
    let output = session.run(&[]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        session.operations(),
        [
            "digest",
            "firmware",
            "firmware",
            "firmware",
            "clock",
            "hold",
            "clock",
            "play",
            "wait_until",
            "status",
            "release",
            "firmware",
            "firmware",
            "firmware",
            "clock",
            "hold",
            "clock",
            "play",
            "reply",
            "wait_until",
            "release"
        ]
    );
    let ledger = session.ledger();
    assert_eq!(
        ledger[9]["args"],
        json!(["session", "status", "fixture-first", "--json"])
    );
    assert_eq!(ledger[17]["args"][4], "fixture-second-lease");
    assert_eq!(
        session.report()["bench_session_ids"],
        json!(["batch-hold-1", "batch-hold-2"])
    );
    assert_eq!(
        events(&session),
        [
            "run_started",
            "step_started",
            "hold_opened",
            "preempted",
            "hold_closed",
            "retry_started",
            "hold_opened",
            "step_completed",
            "hold_closed",
            "run_completed"
        ]
    );
}

#[test]
fn f10_second_preemption_fails_without_a_third_hold_or_retry() {
    let session = Session::new(&json!({"preemptions":2,"holds":holds()}));
    failed(&session.run(&[]), "preempted");
    assert_eq!(
        session.operations(),
        [
            "digest",
            "firmware",
            "firmware",
            "firmware",
            "clock",
            "hold",
            "clock",
            "play",
            "wait_until",
            "status",
            "release",
            "firmware",
            "firmware",
            "firmware",
            "clock",
            "hold",
            "clock",
            "play",
            "wait_until",
            "status",
            "release"
        ]
    );
    assert_eq!(
        session.report()["checks"],
        json!({"step_0":false,"session_released":true})
    );
    assert_eq!(events(&session).iter().filter(|e| *e == "retry_started").count(), 1);
}

#[test]
fn f11_cancel_kills_stalled_child_without_dispatching_unsafe_cleanup() {
    let session = Session::new(&json!({"cancel_on":"play"}));
    session.scenario("two.yaml", &json!([{"say":"utterance.wav"}]));
    failed(
        &session.run(&["two.yaml", "--cancel-file", "cancel.signal"]),
        "cancelled; child reaped",
    );
    assert_eq!(
        session.operations(),
        [
            "digest", "firmware", "firmware", "firmware", "clock", "hold", "clock", "play"
        ]
    );
    assert_eq!(
        session.report()["checks"],
        json!({"step_0":false,"session_released":false})
    );
    assert_eq!(events(&session).last().unwrap(), "run_failed");
}

#[test]
fn f11_primary_and_cleanup_errors_are_both_visible() {
    let session = Session::new(&json!({"cancel_on":"play"}));
    let output = session.run(&["--cancel-file", "cancel.signal"]);
    failed(
        &output,
        "cancelled; child reaped; cleanup: port session release failed: playback completion unknown; coordinator recovery required",
    );
    assert_eq!(
        session.report()["checks"],
        json!({"step_0":false,"session_released":false})
    );
    assert_eq!(session.operations().iter().filter(|s| *s == "release").count(), 0);
}

#[test]
fn ordinary_failure_or_untrusted_preemption_status_never_retries() {
    for status in [
        json!({"holds":[]}),
        json!({"holds":[{"id":"other-hold","ending":{"reason":"preempted"}}]}),
    ] {
        let session = Session::new(&json!({"preemptions":1,"status":status}));
        failed(&session.run(&[]), "play:");
        assert_eq!(session.operations().iter().filter(|s| *s == "hold").count(), 1);
        assert_eq!(session.report()["result"], "fail");
    }
}

#[test]
fn renewal_and_retry_recheck_quiet_hours_without_repeating_chirp() {
    for fixture in [
        json!({"times":[0,600_000,625_000],"hours":[9,9,8]}),
        json!({"preemptions":1,"hours":[9,9,8]}),
    ] {
        let session = Session::new(&fixture);
        failed(&session.run(&[]), "before 09:00"); // lint-rules: allow-literal expected renewal/retry quiet-hours diagnostic
        assert_eq!(session.operations().iter().filter(|s| *s == "hold").count(), 1);
        assert_eq!(session.operations().iter().filter(|s| *s == "chirp").count(), 0);
        assert_eq!(session.operations().iter().filter(|s| *s == "release").count(), 1);
        assert_eq!(session.report()["result"], "fail");
    }
}

#[test]
fn failed_rotation_release_never_reopens_or_claims_cleanup_success() {
    let session = Session::new(&json!({"times":[0,600_000],"fail":"release"}));
    failed(&session.run(&[]), "release failed");
    assert_eq!(
        session.operations(),
        [
            "digest", "firmware", "firmware", "firmware", "clock", "hold", "clock", "release"
        ]
    );
    assert_eq!(session.report()["checks"]["session_released"], false);
}

#[test]
fn f12_release_requires_confirmation_for_the_matching_ended_hold() {
    for release in [
        json!({}),
        json!({"holds":[{"id":"fixture-hold","state":"live"}]}),
        json!({"holds":[{"id":"other-hold","state":"ended"}]}),
    ] {
        let session = Session::new(&json!({"release":release}));
        failed(&session.run(&[]), "release did not confirm");
        assert_eq!(
            session.report()["checks"],
            json!({"step_0":true,"session_released":false})
        );
        assert_eq!(session.operations().iter().filter(|s| *s == "release").count(), 1);
    }
}

// T3: response loss after a synthetic queue must never authorize another held command.
#[test]
fn b0_lost_play_response_retains_unknown_completion_and_primary_error() {
    for frame in [false, true] {
        for (fixture, primary) in [
            (
                json!({"fail":"play"}),
                "selected tool failed or exceeded output limit; child output discarded",
            ),
            (json!({"cancel_on":"play"}), "run cancelled; child reaped"),
            (json!({"malformed":"play"}), "port: malformed JSON"),
            (
                json!({"oversized":"play"}),
                "selected tool failed or exceeded output limit; child output discarded",
            ),
        ] {
            let session = Session::new(&fixture);
            let mut steps = vec![json!({"say":"utterance.wav"})];
            if frame {
                steps.push(json!({"expect_frame":"frame.jpg"}));
            }
            steps.extend([
                json!({"expect_speech_contains":["time"]}),
                json!({"say":"utterance.wav"}),
            ]);
            session.scenario("one.yaml", &json!(steps));
            let output = session.run(&["--cancel-file", "cancel.signal"]);
            failed(&output, primary);
            assert_eq!(
                String::from_utf8(output.stderr).unwrap(),
                format!(
                    "echo-lab: play: {primary}; cleanup: port session release failed: playback completion unknown; coordinator recovery required\n"
                )
            );
            assert_eq!(
                fs::read(session.0.join("play-queued")).unwrap(),
                b"synthetic queued playback"
            );
            assert_eq!(
                session.operations(),
                [
                    "digest", "firmware", "firmware", "firmware", "clock", "hold", "clock", "play"
                ]
            );
            assert_eq!(session.ledger().last().unwrap()["wall_ms"], 1_794_733_200_000_i64);
            assert_eq!(
                session.report()["checks"],
                json!({"step_0":false,"session_released":false})
            );
            assert_eq!(session.report()["result"], "fail");
        }
    }
}
