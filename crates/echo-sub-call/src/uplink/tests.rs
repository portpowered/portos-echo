use super::*;
use crate::{LiveCall, State};
use echo_core_api::{CallEvent, Commands, Event, Now, Subsystem};
use std::{num::NonZeroU64, time::Duration};

fn run(generation: u64) -> SessionHandle {
    SessionHandle {
        index: 0,
        generation: NonZeroU64::new(generation).unwrap(),
    }
}

fn step(state: &mut State, run: SessionHandle, event: UplinkEvent<'_>) -> CallCommands {
    let mut out = Commands::default();
    let now = Now {
        monotonic: Duration::ZERO,
        wall: std::time::UNIX_EPOCH,
    };
    LiveCall::on_event(state, &mut Event::Call(CallEvent::Uplink { run, event }), now, &mut out);
    assert_eq!(LiveCall::check(state), Ok(()));
    out.take_call()
}

fn offer(commands: CallCommands, owner: SessionHandle, marker: i16) -> u64 {
    let (action, frame) = commands.into_parts();
    let Some(CallCommand::SendUplinkFrame { run, attempt }) = action else {
        unreachable!("expected an uplink send command");
    };
    assert_eq!(run, owner);
    assert_eq!(frame, Some([marker; 320]));
    attempt
}

fn receipt(state: &mut State, run: SessionHandle, attempt: u64, receipt: UplinkReceipt) {
    assert_eq!(
        step(state, run, UplinkEvent::Result { attempt, receipt }).into_parts(),
        (None, None)
    );
}

#[test]
fn fixed_ring_evicts_oldest_and_drains_in_order_in_the_original_storage() {
    let mut state = State::default();
    let base = state.uplink.slots.as_ptr();
    assert_eq!(state.uplink.slots.len(), CAPACITY);

    fill_overflow_and_drain(&mut state, run(1));

    assert_eq!(state.uplink.slots.as_ptr(), base);
    assert_eq!(state.uplink.slots.len(), CAPACITY);
}

fn fill_overflow_and_drain(state: &mut State, run: SessionHandle) {
    step(state, run, UplinkEvent::CaptureOpened);
    let first = offer(step(state, run, UplinkEvent::MicFrame(&[0; 320])), run, 0);
    receipt(state, run, first, UplinkReceipt::Full);
    let capacity = i16::try_from(CAPACITY).unwrap();
    for marker in 1..=capacity {
        let out = step(state, run, UplinkEvent::MicFrame(&[marker; 320]));
        assert_eq!(out.uplink_evicted, marker == capacity);
        assert_eq!(out.into_parts(), (None, None));
    }
    assert_eq!(state.view().backlog_depth, CAPACITY);
    assert_eq!(state.view().uplink_dropped, 1);
    for marker in 1..=capacity {
        let attempt = offer(step(state, run, UplinkEvent::Writable), run, marker);
        receipt(state, run, attempt, UplinkReceipt::Accepted);
    }
    assert_eq!(state.view().backlog_depth, 0);
}

#[test]
fn mute_and_warm_run_release_invalidate_offers_without_popping_new_speech() {
    let mut state = State::default();
    let first = run(1);
    let next = run(2);
    step(&mut state, first, UplinkEvent::CaptureOpened);
    let muted = offer(step(&mut state, first, UplinkEvent::MicFrame(&[11; 320])), first, 11);
    assert_eq!(
        step(&mut state, first, UplinkEvent::DropBacklog).into_parts(),
        (Some(CallCommand::DropBacklog(first)), None)
    );
    let after_mute = offer(step(&mut state, first, UplinkEvent::MicFrame(&[22; 320])), first, 22);
    receipt(&mut state, first, muted, UplinkReceipt::Accepted);
    assert_eq!(state.view().backlog_depth, 1);
    receipt(&mut state, first, after_mute, UplinkReceipt::Failed);
    let retry = offer(step(&mut state, first, UplinkEvent::Writable), first, 22);
    assert_eq!(
        step(&mut state, first, UplinkEvent::CaptureClosed).into_parts(),
        (Some(CallCommand::DropBacklog(first)), None)
    );
    assert_eq!(state.view().uplink_run, None);
    assert_eq!(state.view().backlog_depth, 0);
    step(&mut state, next, UplinkEvent::CaptureOpened);
    let active = offer(step(&mut state, next, UplinkEvent::MicFrame(&[33; 320])), next, 33);
    receipt(&mut state, first, retry, UplinkReceipt::Accepted);
    receipt(&mut state, first, active, UplinkReceipt::Accepted);
    assert_eq!(
        step(&mut state, first, UplinkEvent::Writable).into_parts(),
        (None, None)
    );
    assert_eq!(state.view().backlog_depth, 1);
    receipt(&mut state, next, active, UplinkReceipt::Accepted);
    assert_eq!(state.view().backlog_depth, 0);
}
