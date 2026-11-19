//! T3: real delivered CLI, synthetic Port/bridge processes and isolated reports.
use super::{Session, failed};
use serde_json::{Value, json};
use std::io::Write;

fn session(extra: &Value) -> Session {
    let mut fixture = json!({"real_config":true,"device_bridge_fault":"ring_replay"});
    fixture
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    Session::new(&fixture)
}

fn plays(session: &Session) -> usize {
    session.operations().iter().filter(|op| *op == "play").count()
}

fn events(session: &Session) -> Vec<Value> {
    std::fs::read_to_string(session.0.join("artifacts/events.jsonl"))
        .unwrap()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .filter(|row: &Value| {
            matches!(
                row["event"].as_str(),
                Some("talkback_lost_suspected" | "retry_started" | "preempted")
            )
        })
        .collect()
}

fn assert_attempt(session: &Session, count: u8, first: Option<bool>) {
    let report = session.report();
    assert_eq!(
        report["step_attempts"],
        json!([{"step":0,"iteration":0,"attempts":count,"first_try_woke":first}])
    );
    assert_eq!(plays(session), usize::from(count));
    assert_eq!(report["talkback"]["replay_count"], count - 1);
    assert_eq!(report["checks"]["session_released"], true);
    writeln!(
        std::io::stderr(),
        "ring_replay: attempts={count} first_try_woke={first:?} talkback={}",
        report["talkback"]
    )
    .unwrap();
}

#[test]
fn ring_replay_first_miss_then_wake_passes_with_ordered_payload_free_events() {
    let session = session(&json!({"ring_wakes":[false,true],"ring_prior_wake":true}));
    let output = crate::device::configured(&session, &json!([{"say":"utterance.wav"}]))
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_attempt(&session, 2, Some(false));
    let rows = events(&session);
    assert_eq!(
        rows.iter()
            .map(|row| row["event"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["talkback_lost_suspected", "retry_started"]
    );
    for row in rows {
        assert_eq!(row["step"], 0);
        assert_eq!(row["message"], Value::Null);
    }
    assert!(!session.0.join("artifacts/reply-0.wav").exists());
}

#[test]
fn ring_replay_two_text_misses_fail_with_both_contexts_and_no_third_play() {
    let session = session(&json!({"ring_wakes":[false,false]}));
    let output = crate::device::configured(&session, &json!([{"say":"text:synthetic request"}]))
        .args(["--speech-endpoint", "fixture-speech", "--speech-model", "fixture-model"])
        .output()
        .unwrap();
    failed(
        &output,
        "attempt 1: play: no wake observed; attempt 2: play: no wake observed",
    );
    assert_attempt(&session, 2, Some(false));
    assert_eq!(
        events(&session)
            .iter()
            .filter(|row| row["event"] == "talkback_lost_suspected")
            .count(),
        2
    );
    assert_eq!(session.report()["checks"]["step_0"], false);
    assert!(session.0.join("artifacts/reply-0.wav").exists());
}

#[test]
fn ring_replay_positive_first_wake_prevents_reply_or_speech_failure_replay() {
    for extra in [
        json!({}),
        json!({"replies":[{"reply_frames":0}]}),
        json!({"fail":"reply"}),
        json!({"transcript":"different reply"}),
    ] {
        let session = session(&extra);
        let speech = extra.get("transcript").is_some();
        let steps = if speech {
            json!([{"say":"utterance.wav"},{"expect_speech_contains":["time"]}])
        } else {
            json!([{"say":"utterance.wav"}])
        };
        let output = crate::device::configured(&session, &steps).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(i32::from(!extra.as_object().unwrap().is_empty())),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_attempt(&session, 1, Some(true));
        assert_eq!(events(&session), Vec::<Value>::new());
    }
}

#[test]
fn ring_replay_mixed_says_publish_exact_half_first_try_rate() {
    let session = session(&json!({"ring_wakes":[false,true,true]}));
    let output = crate::device::configured(&session, &json!([{"say":"utterance.wav"},{"say":"utterance.wav"}]))
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(plays(&session), 3);
    let report = session.report();
    assert_eq!(
        report["step_attempts"],
        json!([
        {"step":0,"iteration":0,"attempts":2,"first_try_woke":false},
        {"step":1,"iteration":0,"attempts":1,"first_try_woke":true}])
    );
    assert_eq!(
        report["talkback"],
        json!({"eligible_steps":2,"first_try_wakes":1,"first_try_wake_rate":0.5,"replay_count":1})
    );
    writeln!(std::io::stderr(), "ring_replay mixed: plays=3 {}", report["talkback"]).unwrap();
}

#[test]
fn ring_replay_legacy_audio_fallback_is_bounded_and_keeps_miss_visible() {
    for replies in [
        json!([{"reply_frames":0},{"reply_frames":2}]),
        json!([{"reply_frames":0},{"reply_frames":0}]),
    ] {
        let session = Session::new(&json!({"replies":replies}));
        let output = session.run(&[]);
        let pass = replies[1]["reply_frames"] == 2;
        assert_eq!(output.status.code(), Some(i32::from(!pass)));
        assert_attempt(&session, 2, Some(false));
        assert_eq!(
            session.operations().iter().filter(|op| *op == "wake_wait").count(),
            if pass { 1 } else { 2 }
        );
    }
}

#[test]
fn ring_replay_preemption_and_loss_share_one_budget() {
    let session = session(&json!({"preemptions":1,"ring_wakes":[false,false]}));
    let output = crate::device::configured(&session, &json!([{"say":"utterance.wav"}]))
        .output()
        .unwrap();
    failed(&output, "attempt 1: play: preempted; attempt 2: play: no wake observed");
    assert_attempt(&session, 2, None);
    assert_eq!(
        events(&session)
            .iter()
            .map(|row| row["event"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["preempted", "retry_started", "talkback_lost_suspected"]
    );
}

#[test]
fn ring_replay_loss_followed_by_preemption_also_has_only_two_dispatches() {
    let session = session(&json!({"ring_wakes":[false,false],"preempt_plays":[1]}));
    let output = crate::device::configured(&session, &json!([{"say":"utterance.wav"}]))
        .output()
        .unwrap();
    failed(&output, "attempt 1: play: no wake observed; attempt 2: play: preempted");
    assert_attempt(&session, 2, Some(false));
    let outputs: Vec<_> = session
        .ledger()
        .into_iter()
        .filter(|row| row["operation"] == "play")
        .map(|row| {
            let args = row["args"].as_array().unwrap();
            let output = args.iter().position(|arg| arg == "--output").unwrap();
            args[output + 1].as_str().unwrap().to_owned()
        })
        .collect();
    assert_ne!(outputs[0], outputs[1]);
    assert!(std::path::Path::new(&outputs[0]).is_file());
    assert!(!std::path::Path::new(&outputs[1]).exists());
    assert_eq!(
        events(&session)
            .iter()
            .filter(|row| row["event"] == "retry_started")
            .count(),
        1
    );
}

#[test]
fn ring_replay_cancellation_after_confirmed_release_refuses_second_play() {
    let session = session(&json!({"ring_wakes":[false,true],"cancel_on_release":true}));
    let output = crate::device::configured(&session, &json!([{"say":"utterance.wav"}]))
        .args(["--cancel-file", "cancel.signal"])
        .output()
        .unwrap();
    failed(&output, "cancelled");
    assert_eq!(plays(&session), 1);
    assert_eq!(session.report()["step_attempts"][0]["attempts"], 1);
    assert_eq!(session.report()["step_attempts"][0]["first_try_woke"], false);
}

#[test]
fn ring_replay_late_wake_during_release_refuses_second_play_for_either_retry_reason() {
    for preemptions in [0, 1] {
        let session = session(&json!({
            "ring_wakes":[false,true], "ring_wake_on_release":true, "preemptions":preemptions
        }));
        let output = crate::device::configured(&session, &json!([{"say":"utterance.wav"}]))
            .output()
            .unwrap();
        failed(&output, "wake observed before replay; replay refused");
        assert_attempt(&session, 1, (preemptions == 0).then_some(false));
        assert_eq!(session.report()["checks"]["step_0"], false);
        let rows = events(&session);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0]["event"],
            if preemptions == 0 {
                "talkback_lost_suspected"
            } else {
                "preempted"
            }
        );
        assert_eq!(rows[1]["event"], "retry_started");
    }
}

#[test]
fn ring_replay_first_text_wake_is_never_replayed() {
    let session = session(&json!({}));
    let output = crate::device::configured(&session, &json!([{"say":"text:synthetic request"}]))
        .args(["--speech-endpoint", "fixture-speech", "--speech-model", "fixture-model"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_attempt(&session, 1, Some(true));
    assert_eq!(events(&session), Vec::<Value>::new());
}

#[test]
fn ring_replay_replayed_wake_with_failed_reply_stays_failed_and_keeps_both_contexts() {
    let session = session(&json!({"ring_wakes":[false,true],"replies":[{"reply_frames":2},{"reply_frames":0}]}));
    let output = crate::device::configured(&session, &json!([{"say":"utterance.wav"}]))
        .output()
        .unwrap();
    failed(
        &output,
        "attempt 1: play: no wake observed; attempt 2: play: no post-playback reply audio",
    );
    assert_attempt(&session, 2, Some(false));
}

#[test]
fn ring_replay_failed_release_and_quiet_hours_prevent_second_dispatch() {
    for extra in [json!({"fail":"release"}), json!({"hours":[9,9,8]})] {
        let mut fixture = extra;
        fixture["ring_wakes"] = json!([false, true]);
        let session = session(&fixture);
        let output = crate::device::configured(&session, &json!([{"say":"utterance.wav"}]))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(plays(&session), 1);
        assert_eq!(session.report()["step_attempts"][0]["attempts"], 1);
        assert_eq!(session.report()["step_attempts"][0]["first_try_woke"], false);
    }
}

#[test]
fn ring_replay_broken_trace_or_reply_measurement_never_becomes_loss() {
    for extra in [
        json!({"ring_malformed_trace":true}),
        json!({"replies":[{}]}),
        json!({"fail":"reply"}),
    ] {
        let session = session(&extra);
        let output = crate::device::configured(&session, &json!([{"say":"utterance.wav"}]))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(plays(&session), 1);
        assert_eq!(events(&session), Vec::<Value>::new());
    }
}

#[test]
fn ring_replay_preflight_refusal_does_not_fabricate_an_attempt() {
    let session = Session::new(&json!({"hour":8}));
    failed(&session.run(&[]), "quiet hours");
    let report = session.report();
    assert!(report.get("step_attempts").is_none());
    assert!(report.get("talkback").is_none());
    assert_eq!(plays(&session), 0);
}

#[test]
fn ring_replay_host_deadline_uses_play_end_and_only_the_remaining_window() {
    let helper = include_str!("../../src/lab_run/run_host.py");
    let script = format!(
        r#"
import datetime,sys
from unittest.mock import patch
ns={{"__name__":"test"}}
exec({helper:?},ns)
base=datetime.datetime(2026,11,15,9,tzinfo=datetime.timezone.utc)
state={{"now":base,"waits":[]}}
class Clock(datetime.datetime):
    @classmethod
    def now(cls,tz=None): return state["now"]
def advance(seconds):
    state["waits"].append(seconds)
    state["now"] += datetime.timedelta(seconds=seconds)
def call(operation):
    sys.argv=["host",operation,"2026-11-15T09:00:00Z"]
    return ns["main"]()
with patch.object(ns["datetime"],"datetime",Clock), patch.object(ns["time"],"sleep",advance):
    assert call("wake_remaining")=={{"remaining_ms":6000}}
    state["now"]=base+datetime.timedelta(seconds=3.8)
    assert call("wake_remaining")=={{"remaining_ms":2200}}
    state["now"]=base+datetime.timedelta(seconds=6)
    assert call("wake_remaining")=={{"remaining_ms":0}}
    state["now"]=base+datetime.timedelta(seconds=10)
    assert call("wake_remaining")=={{"remaining_ms":0}}
    assert call("wake_wait")=={{"reached":True}}
    assert state["waits"]==[]
print("remaining_ms=[6000,2200,0,0]; capture beyond deadline adds no wait")
"#
    );
    let output = std::process::Command::new("python")
        .args(["-c", &script])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "remaining_ms=[6000,2200,0,0]; capture beyond deadline adds no wait"
    );
}
