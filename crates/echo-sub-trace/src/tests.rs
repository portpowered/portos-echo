use super::*;
use echo_core_api::{Clock, PausedClock, TraceRow};
use std::time::UNIX_EPOCH;

fn row(id: usize) -> Event {
    Event::Trace(Some(PendingTrace {
        row: TraceRow {
            event: "fixture".into(),
            elapsed_ms: 0,
            mode: "api_key",
            run: Some(id.to_string()),
            fields: std::iter::empty().collect(),
            payload_fields: &[],
            limit: 4096,
        },
        receipt: None,
    }))
}

fn fill(state: &mut State, clock: &PausedClock, out: &mut Commands) {
    for id in 0..CAPACITY {
        let mut event = row(id);
        Trace::on_event(state, &mut event, clock.now(), out);
        assert!(matches!(event, Event::Trace(None)));
    }
}

#[test]
fn full_ring_rejects_without_eviction_and_drains_in_admission_order() {
    let mut state = State::default();
    let clock = PausedClock::new(UNIX_EPOCH);
    let mut out = Commands::default();
    fill(&mut state, &clock, &mut out);
    let mut overflow = row(CAPACITY);
    Trace::on_event(&mut state, &mut overflow, clock.now(), &mut out);
    assert!(matches!(overflow, Event::Trace(Some(_))));
    assert!(!out.take_trace_wake());
    assert_eq!(Trace::next_deadline(&state), Some(TICK));
    Trace::on_tick(&mut state, clock.now(), &mut out);
    assert!(out.take_trace_wake());
    state.close();
    for id in 0..CAPACITY {
        assert_eq!(state.next_row().unwrap().row.run, Some(id.to_string()));
    }
    Trace::on_event(&mut state, &mut overflow, clock.now(), &mut out);
    assert!(matches!(overflow, Event::Trace(Some(_))));
    assert!(state.next_row().is_none());
    assert_eq!(Trace::check(&state), Ok(()));
    assert_eq!(Trace::next_deadline(&state), None);
}
