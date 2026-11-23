use super::{Controller, SessionPort, session_error};
use crate::Error;
use echo_core_api::{
    Event, SessionChange, SessionCommands, SessionEvent, SessionKnobs, SessionMark, SessionOutcome, SessionPlayback,
    SessionTurnEvent, SessionWait,
};
use tokio::time::Instant;

impl Controller {
    pub(crate) fn configure_session(&self, knobs: SessionKnobs) -> Result<(), Error> {
        let outcome = self
            .session_step(&mut self.state(), SessionEvent::Configure(knobs))
            .outcome;
        match outcome {
            Some(Ok(SessionOutcome::Applied)) => Ok(()),
            _ => Err(session_error(outcome)),
        }
    }
}

impl SessionPort {
    pub(crate) fn turn(&self, change: SessionTurnEvent) -> SessionCommands {
        self.turn_step(&mut Event::Session(SessionEvent::Turn {
            session: self.handle,
            change,
        }))
    }

    // World observes the existing allocation and the trace boundary receives it
    // back. The reducer retains only the bounded excerpt while barge is armed.
    pub(crate) fn transcript(&self, text: String, whole: bool, sounded: bool, voiced: bool) -> (String, bool) {
        let mut event = Event::Session(SessionEvent::Turn {
            session: self.handle,
            change: SessionTurnEvent::Transcript {
                text,
                whole,
                sounded,
                voiced,
            },
        });
        let held = self.turn_step(&mut event).outcome == Some(Ok(SessionOutcome::Held));
        if let Event::Session(SessionEvent::Turn {
            change: SessionTurnEvent::Transcript { text, .. },
            ..
        }) = event
        {
            (text, held)
        } else {
            (String::new(), held)
        }
    }

    fn turn_step(&self, event: &mut Event) -> SessionCommands {
        let mut state = self.world.state();
        let session = self.handle;
        if state.runs.get(session.index, session.generation).is_none() {
            return SessionCommands::default();
        }
        self.world.session_envelope(&mut state, event)
    }

    pub(crate) fn stale(&self) -> bool {
        self.read(echo_sub_session::State::stale)
    }

    pub(crate) fn dispatch_in_flight(&self) -> bool {
        self.read(echo_sub_session::State::dispatch_in_flight)
    }

    pub(crate) fn dispatch_waiting(&self) -> bool {
        self.read(echo_sub_session::State::dispatch_waiting)
    }

    pub(crate) fn dispatch_held(&self) -> bool {
        self.read(echo_sub_session::State::dispatch_held)
    }

    pub(crate) fn take_marks(&self) -> Vec<SessionMark> {
        self.world.state().world.pacer.session.take_marks()
    }

    pub(crate) fn barge_pending(&self) -> bool {
        self.read(echo_sub_session::State::barge_pending)
    }

    pub(crate) fn barge_due(&self, finished: bool) -> Option<Instant> {
        let state = self.world.state();
        let clock = &state.world.clock;
        let now = clock.start.checked_add(clock.start.elapsed())?;
        state
            .world
            .pacer
            .session
            .barge_due(now.into_std(), finished)
            .map(Instant::from_std)
    }

    pub(crate) fn reply_wait(
        &self,
        played: Option<Instant>,
        finished: bool,
        voiced: bool,
    ) -> (Option<Instant>, SessionWait) {
        let playback = SessionPlayback {
            played_due: played.map(Instant::into_std),
            finished,
            voiced,
        };
        let mut state = self.world.state();
        if !state.world.pacer.session.playback_matches(playback) {
            self.world.session_step(
                &mut state,
                SessionEvent::Run {
                    session: self.handle,
                    change: SessionChange::Playback(playback),
                },
            );
        }
        let (due, wait) = state
            .world
            .pacer
            .session
            .reply_wait(playback.played_due, finished, voiced);
        (due.map(Instant::from_std), wait)
    }

    pub(crate) fn listening_wait(&self, idle: Instant) -> (Option<Instant>, SessionWait) {
        let (due, wait) = self.read(|state| state.listening_wait(idle.into_std()));
        (due.map(Instant::from_std), wait)
    }

    pub(crate) fn wait_expired(
        &self,
        wait: SessionWait,
        due: Option<Instant>,
        voiced: bool,
        listening: bool,
    ) -> Result<SessionOutcome, Error> {
        let outcome = self
            .dispatch(SessionChange::WaitExpired {
                wait,
                due: due.map(Instant::into_std),
                voiced,
                listening,
            })
            .outcome;
        match outcome {
            Some(Ok(outcome)) => Ok(outcome),
            _ => Err(session_error(outcome)),
        }
    }
}
