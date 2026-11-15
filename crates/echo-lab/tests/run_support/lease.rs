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
fn f08_rotate_before_ceiling_wait_exactly_25_seconds_and_chirp_only_once() {
    let session = Session::new(&json!({"times":[0,0,0,659_000,684_000,684_000],"holds":holds()}));
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
            "digest", "firmware", "clock", "hold", "clock", "chirp", "reply", "clock", "play", "reply", "firmware",
            "clock", "release", "wait", "clock", "hold", "clock", "play", "reply", "release"
        ]
    );
    let ledger = session.ledger();
    assert_eq!(
        ledger[12]["args"],
        json!(["session", "release", "fixture-first", "--json"])
    );
    assert_eq!(ledger[13]["args"], json!(["-c", "<host-helper>", "wait", "25000"]));
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
            "digest", "firmware", "clock", "hold", "clock", "chirp", "reply", "clock", "play", "status", "release",
            "firmware", "clock", "hold", "clock", "play", "reply", "release"
        ]
    );
    let ledger = session.ledger();
    assert_eq!(
        ledger[9]["args"],
        json!(["session", "status", "fixture-first", "--json"])
    );
    assert_eq!(ledger[15]["args"][4], "fixture-second-lease");
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
            "digest", "firmware", "clock", "hold", "clock", "chirp", "reply", "clock", "play", "status", "release",
            "firmware", "clock", "hold", "clock", "play", "status", "release"
        ]
    );
    assert_eq!(
        session.report()["checks"],
        json!({"step_0":false,"session_released":true})
    );
    assert_eq!(events(&session).iter().filter(|e| *e == "retry_started").count(), 1);
}

#[test]
fn f11_cancel_kills_stalled_child_and_releases_once_without_running_next_step() {
    let session = Session::new(&json!({"cancel_on":"play"}));
    session.scenario("two.yaml", &json!([{"say":"utterance.wav"}]));
    failed(
        &session.run(&["two.yaml", "--cancel-file", "cancel.signal"]),
        "cancelled; child reaped",
    );
    assert_eq!(
        session.operations(),
        [
            "digest", "firmware", "clock", "hold", "clock", "chirp", "reply", "clock", "play", "release"
        ]
    );
    assert_eq!(
        session.report()["checks"],
        json!({"step_0":false,"session_released":true})
    );
    assert_eq!(events(&session).last().unwrap(), "run_failed");
}

#[test]
fn f11_primary_and_cleanup_errors_are_both_visible() {
    let session = Session::new(&json!({"cancel_on":"play","fail":"release"}));
    let output = session.run(&["--cancel-file", "cancel.signal"]);
    failed(&output, "cancelled; child reaped; cleanup: port session release failed");
    assert_eq!(
        session.report()["checks"],
        json!({"step_0":false,"session_released":false})
    );
    assert_eq!(session.operations().iter().filter(|s| *s == "release").count(), 1);
}

#[test]
fn ordinary_failure_or_untrusted_preemption_status_never_retries() {
    for status in [
        json!({"holds":[]}),
        json!({"holds":[{"id":"other-hold","ending":{"reason":"preempted"}}]}),
    ] {
        let session = Session::new(&json!({"fail":"play","status":status}));
        failed(&session.run(&[]), "play:");
        assert_eq!(session.operations().iter().filter(|s| *s == "hold").count(), 1);
        assert_eq!(session.report()["result"], "fail");
    }
}

#[test]
fn renewal_and_retry_recheck_quiet_hours_without_repeating_chirp() {
    for fixture in [
        json!({"times":[0,0,659_000,684_000],"hours":[9,9,9,8]}),
        json!({"preemptions":1,"hours":[9,9,9,8]}),
    ] {
        let session = Session::new(&fixture);
        failed(&session.run(&[]), "before 09:00");
        assert_eq!(session.operations().iter().filter(|s| *s == "hold").count(), 1);
        assert_eq!(session.operations().iter().filter(|s| *s == "chirp").count(), 1);
        assert_eq!(session.operations().iter().filter(|s| *s == "release").count(), 1);
        assert_eq!(session.report()["result"], "fail");
    }
}

#[test]
fn failed_rotation_release_never_reopens_or_claims_cleanup_success() {
    let session = Session::new(&json!({"times":[0,0,659_000],"fail":"release"}));
    failed(&session.run(&[]), "release failed");
    assert_eq!(
        session.operations(),
        [
            "digest", "firmware", "clock", "hold", "clock", "chirp", "reply", "clock", "release"
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
