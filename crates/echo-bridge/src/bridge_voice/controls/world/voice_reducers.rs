use echo_core_api::{Broken, CommandKind, Commands, Event, EventKind, Now, Subsystem};
use echo_sub_pacer::Pacer;
use echo_sub_session::Session;
use std::time::Duration;

pub(super) struct VoiceReducers;

pub(super) struct State {
    pub(super) playback: echo_sub_pacer::State,
    pub(super) session: echo_sub_session::State,
}

// The generic World has one voice slot. Keep each reducer's state and timer
// independent while stepping session before pacer. The call slot follows pacer.
impl Subsystem for VoiceReducers {
    type State = State;
    const INPUTS: &'static [EventKind] = &[EventKind::Session, EventKind::Pacer];
    const OUTPUTS: &'static [CommandKind] = &[
        CommandKind::StartCapture,
        CommandKind::StopCapture,
        CommandKind::EndSession,
        CommandKind::CutReply,
        CommandKind::OpenCall,
        CommandKind::CloseCall,
        CommandKind::SessionToolDecision,
        CommandKind::WriteFrame,
        CommandKind::LipBatch,
        CommandKind::MouthChanged,
        CommandKind::PlayoutFinished,
    ];

    fn on_event(state: &mut State, event: &mut Event, now: Now, out: &mut Commands) {
        Session::on_event(&mut state.session, event, now, out);
        Pacer::on_event(&mut state.playback, event, now, out);
    }

    fn on_tick(state: &mut State, now: Now, out: &mut Commands) {
        Pacer::on_tick(&mut state.playback, now, out);
    }

    fn next_deadline(state: &State) -> Option<Duration> {
        let session = Session::next_deadline(&state.session);
        let pacer = Pacer::next_deadline(&state.playback);
        session.zip(pacer).map(|(a, b)| a.min(b)).or(session).or(pacer)
    }

    fn check(state: &State) -> Result<(), Broken> {
        Session::check(&state.session)?;
        Pacer::check(&state.playback)
    }
}
