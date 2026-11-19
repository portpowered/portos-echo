use super::*;
use echo_contracts::wyoming::{Event, Header};

fn receive(ledger: &mut Ledger, direction: &'static str, kind: &str, at: u128) {
    let payload = if kind == "audio-chunk" { vec![128, 0] } else { vec![] };
    ledger
        .observe(
            direction,
            &Event {
                header: Header {
                    r#type: kind.into(),
                    data: None,
                    data_length: 0,
                    payload_length: payload.len(),
                },
                data: None,
                payload,
            },
            at,
        )
        .unwrap();
}

fn reply(ledger: &mut Ledger, at: u128) {
    for (direction, kind, delta) in [
        ("playback", "audio-start", 0),
        ("playback", "audio-chunk", 1),
        ("playback", "audio-stop", 2),
        ("capture", "played", 3),
    ] {
        receive(ledger, direction, kind, at + delta);
    }
}

fn idle(end: u128) -> Ledger {
    let mut ledger = Ledger::default();
    receive(&mut ledger, "capture", "run-pipeline", 0);
    reply(&mut ledger, 999_997);
    receive(&mut ledger, "playback", "hassmic-end", end);
    ledger
}

#[test]
fn device_budget_idle_accepts_inclusive_100ms_and_reports_measured_101ms() {
    let expected = Expected {
        replies: 1,
        listen_seconds: 3,
    };
    for end in [3_900_000, 4_000_000, 4_100_000] {
        let measured = expected.check(&idle(end)).unwrap();
        assert_eq!(measured["last_activity_played_us"], 1_000_000);
        assert_eq!(
            measured["idle_after_played_us"],
            u64::try_from(end - 1_000_000).unwrap()
        );
    }
    for (end, offset) in [(3_899_000, "-101000"), (4_101_000, "+101000")] {
        assert_eq!(
            expected.check(&idle(end)),
            Err(format!(
                "device: session idle expected 3000 ms +/-100 ms, observed {} us; offset={offset} us",
                end - 1_000_000
            ))
        );
    }
}

#[test]
fn device_budget_idle_uses_second_reply_activity_and_requires_intervening_capture() {
    let expected = Expected {
        replies: 2,
        listen_seconds: 3,
    };
    let mut ledger = {
        // Assemble an ongoing run rather than recycling the ended one.
        let mut ongoing = Ledger::default();
        receive(&mut ongoing, "capture", "run-pipeline", 0);
        reply(&mut ongoing, 999_997);
        receive(&mut ongoing, "capture", "audio-chunk", 1_100_000);
        reply(&mut ongoing, 1_999_997);
        receive(&mut ongoing, "playback", "hassmic-end", 5_000_000);
        ongoing
    };
    let measured = expected.check(&ledger).unwrap();
    assert_eq!(measured["last_activity_played_us"], 2_000_000);
    assert_eq!(measured["idle_after_played_us"], 3_000_000);
    assert_eq!(measured["capture_chunks_between_replies"], 1);
    ledger
        .receipts
        .retain(|r| !(r.direction == "capture" && r.kind == "audio-chunk"));
    assert_eq!(
        expected.check(&ledger),
        Err("device: unmet prerequisite: capture between session replies".into())
    );
}

#[test]
fn device_budget_idle_missing_late_or_wrong_identity_sources_fail_closed() {
    let expected = Expected {
        replies: 1,
        listen_seconds: 3,
    };
    let mut missing = Ledger::default();
    receive(&mut missing, "capture", "run-pipeline", 0);
    receive(&mut missing, "playback", "audio-start", 1);
    receive(&mut missing, "playback", "audio-chunk", 2);
    receive(&mut missing, "playback", "audio-stop", 3);
    receive(&mut missing, "playback", "hassmic-end", 4_000_000);
    assert_eq!(
        expected.check(&missing),
        Err("device: unmet prerequisite: reply Played".into())
    );
    let mut ledger = idle(4_000_000);
    ledger.receipts.retain(|r| r.kind != "hassmic-end");
    assert_eq!(
        expected.check(&ledger),
        Err("device: unmet prerequisite: one hassmic-end receipt".into())
    );
    let mut late = idle(4_000_000);
    receive(&mut late, "capture", "played", 4_000_001);
    assert_eq!(
        expected.check(&late),
        Err("device: unmet prerequisite: one complete wire session identity".into())
    );
    late.failure = Some("device: malformed wire frame".into());
    assert_eq!(expected.check(&late), Err("device: malformed wire frame".into()));
}

#[test]
fn device_budget_idle_rejects_backwards_end_and_duplicate_or_new_run() {
    let expected = Expected {
        replies: 1,
        listen_seconds: 3,
    };
    let mut backwards = idle(4_000_000);
    backwards.receipts.last_mut().unwrap().elapsed_us = 999_999;
    assert_eq!(
        expected.check(&backwards),
        Err("device: unmet prerequisite: ordered idle end".into())
    );
    for kind in ["hassmic-end", "run-pipeline"] {
        let mut ledger = idle(4_000_000);
        let direction = if kind == "hassmic-end" { "playback" } else { "capture" };
        receive(&mut ledger, direction, kind, 4_000_001);
        // A duplicate end has no active identity; it must still be refused.
        assert!(expected.check(&ledger).is_err());
    }
}
