//! Live call lifecycle and receive-generation policy.
use echo_core_api::{
    Broken, CallCommand, CallCommands, CallEvent, CallHandle, CallPhase, CallReceive, CallSetupStage, CallView,
    CommandKind, Commands, Event, EventKind, Now, Subsystem,
};
use std::{num::NonZeroU64, time::Duration};

pub struct LiveCall;

#[derive(Default)]
pub struct State {
    view: CallView,
}

impl State {
    #[must_use]
    pub fn view(&self) -> CallView {
        self.view
    }

    fn attach(&mut self) {
        let generation = match self.view.handle {
            None => NonZeroU64::MIN,
            Some(CallHandle(previous)) if self.view.phase == CallPhase::Closed => {
                let Some(next) = previous.checked_add(1) else { return };
                next
            }
            Some(_) => return,
        };
        self.view = CallView {
            handle: Some(CallHandle(generation)),
            ..CallView::default()
        };
    }

    fn apply(&mut self, event: CallEvent<'_>, out: &mut CallCommands) {
        use CallEvent::{
            BumpReceiveGeneration, CloseRequested, OpenRequested, PeerAttached, PeerClosed, PeerConnected, PeerFailed,
            ReceiveReset, RemoteAudioFrame, RetireReceive, SetupStage,
        };
        let Some(active) = self.view.handle else {
            if matches!(event, PeerAttached) {
                self.attach();
            }
            return;
        };
        match event {
            PeerAttached => self.attach(),
            OpenRequested { call } if call == active && self.view.phase == CallPhase::Fresh => {
                self.view.phase = CallPhase::Opening;
                out.emit(CallCommand::OpenPeer(call), None);
            }
            SetupStage { call, stage } if call == active && self.view.phase == CallPhase::Opening => {
                self.advance_setup(stage);
            }
            PeerConnected { call }
                if call == active
                    && self.view.phase == CallPhase::Opening
                    && self.view.setup_stage == CallSetupStage::AnswerApplied =>
            {
                self.view.phase = CallPhase::Connected;
                self.view.setup_stage = CallSetupStage::MediaReady;
            }
            PeerFailed { call } | CloseRequested { call } if call == active => self.begin_close(call, out),
            PeerClosed { call } if call == active && self.view.phase == CallPhase::Closing => {
                self.view.phase = CallPhase::Closed;
                self.view.receive = CallReceive::Resetting;
            }
            RetireReceive { call }
                if call == active
                    && self.view.phase == CallPhase::Connected
                    && self.view.receive == CallReceive::Current =>
            {
                self.view.receive = CallReceive::Retired;
            }
            BumpReceiveGeneration { .. } | ReceiveReset { .. } | RemoteAudioFrame { .. } => {
                self.receive(event, active, out);
            }
            _ => (),
        }
    }

    fn advance_setup(&mut self, stage: CallSetupStage) {
        use CallSetupStage::{AnswerApplied, AnswerReceived, New, SignalingStarted};
        if matches!(
            (self.view.setup_stage, stage),
            (New, SignalingStarted) | (SignalingStarted, AnswerReceived) | (AnswerReceived, AnswerApplied)
        ) {
            self.view.setup_stage = stage;
        }
    }

    fn begin_close(&mut self, call: CallHandle, out: &mut CallCommands) {
        if matches!(self.view.phase, CallPhase::Closing | CallPhase::Closed) {
            return;
        }
        self.view.phase = CallPhase::Closing;
        out.emit(CallCommand::ClosePeer(call), None);
    }

    fn receive(&mut self, event: CallEvent<'_>, active: CallHandle, out: &mut CallCommands) {
        if self.view.phase != CallPhase::Connected {
            return;
        }
        match event {
            CallEvent::BumpReceiveGeneration { call }
                if call == active && self.view.receive == CallReceive::Retired =>
            {
                self.view.receive_generation = self.view.receive_generation.wrapping_add(1);
                self.view.receive = CallReceive::Resetting;
                out.emit(
                    CallCommand::ResetReceive {
                        call,
                        generation: self.view.receive_generation,
                    },
                    None,
                );
            }
            CallEvent::ReceiveReset { call, generation }
                if call == active
                    && generation == self.view.receive_generation
                    && self.view.receive == CallReceive::Resetting =>
            {
                self.view.receive = CallReceive::Current;
            }
            CallEvent::RemoteAudioFrame {
                call,
                generation,
                frame,
            } if call == active
                && generation == self.view.receive_generation
                && self.view.receive != CallReceive::Resetting =>
            {
                out.emit(CallCommand::DeliverRemoteAudio { call, generation }, Some(*frame));
            }
            _ => (),
        }
    }
}

impl Subsystem for LiveCall {
    type State = State;
    const INPUTS: &'static [EventKind] = &[EventKind::Call];
    const OUTPUTS: &'static [CommandKind] = &[
        CommandKind::OpenPeer,
        CommandKind::ClosePeer,
        CommandKind::ResetReceive,
        CommandKind::DeliverRemoteAudio,
    ];

    fn on_event(state: &mut State, event: &mut Event<'_>, _now: Now, out: &mut Commands) {
        if let Event::Call(event) = event {
            state.apply(*event, out.call());
        }
    }

    fn next_deadline(_state: &State) -> Option<Duration> {
        None
    }

    fn check(state: &State) -> Result<(), Broken> {
        if state.view.phase == CallPhase::Closed && state.view.receive == CallReceive::Resetting {
            return Err(Broken {
                field: "call.receive_state",
                expected: "no reset after close",
                actual: "reset pending",
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
