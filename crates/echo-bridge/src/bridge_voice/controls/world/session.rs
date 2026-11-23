use super::{Controller, State, pacer};
use crate::Error;
use echo_core_api::{
    Commands, Event, FollowUp, SessionChange, SessionCommand, SessionCommands, SessionError, SessionEvent,
    SessionHandle, SessionOutcome, SessionTimer, SessionView,
};
use std::{fmt, future::Future, sync::Arc, time::Duration};
use tokio::time::Instant;

/// The service's lease on the active run. The slot is released on all exits,
/// including cancellation and a failed playback or device acknowledgement.
pub(crate) struct SessionPort {
    world: Arc<Controller>,
    handle: SessionHandle,
}

impl fmt::Debug for SessionPort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.world.state().world.pacer.session, formatter)
    }
}

impl Controller {
    fn session_step(&self, state: &mut State, event: SessionEvent) -> SessionCommands {
        let mut out = Commands::default();
        state.world.step(&mut Event::Session(event), &mut out);
        self.check_step(state);
        out.take_session()
    }

    fn admit(self: Arc<Self>, opened: Instant, max: Option<Duration>) -> Result<SessionPort, Error> {
        let mut state = self.state();
        let (index, generation) = state.runs.insert(()).ok_or(Error::Control)?;
        let session = SessionHandle { index, generation };
        let event = SessionEvent::Wake {
            session,
            epoch: state.world.clock.start.into_std(),
            opened: opened.into_std(),
            max,
        };
        let result = self.session_step(&mut state, event);
        if result.effect != Some(SessionCommand::StartCapture { session }) {
            state.runs.remove(index, generation);
            return Err(session_error(result.outcome));
        }
        drop(state);
        Ok(SessionPort {
            world: self,
            handle: session,
        })
    }
}

/// Send call lifecycle through the same ordered World before polling the I/O.
/// The call owner keeps its existing setup, retention and teardown fences.
pub(crate) async fn call_effect<T>(open: bool, work: impl Future<Output = Result<T, Error>>) -> Result<T, Error> {
    let (event, command) = if open {
        (SessionEvent::CallAdmitted, SessionCommand::OpenCall)
    } else {
        (SessionEvent::CallCloseDue, SessionCommand::CloseCall)
    };
    let world = pacer::current_controller().unwrap_or_else(Controller::new);
    let out = world.session_step(&mut world.state(), event);
    if out.effect != Some(command) {
        return Err(session_error(out.outcome));
    }
    work.await
}

impl SessionPort {
    pub(crate) fn open(opened: Instant, max: Option<Duration>) -> Result<Self, Error> {
        pacer::current_controller()
            .unwrap_or_else(Controller::new)
            .admit(opened, max)
    }

    fn dispatch(&self, change: SessionChange) -> SessionCommands {
        let mut state = self.world.state();
        let session = self.handle;
        if state.runs.get(session.index, session.generation).is_none() {
            return SessionCommands::default();
        }
        self.world
            .session_step(&mut state, SessionEvent::Run { session, change })
    }

    fn apply(&self, change: SessionChange) -> Result<(), Error> {
        match self.dispatch(change).outcome {
            Some(Ok(SessionOutcome::Applied)) => Ok(()),
            outcome => Err(session_error(outcome)),
        }
    }

    fn read<T>(&self, read: impl FnOnce(&echo_sub_session::State) -> T) -> T {
        read(&self.world.state().world.pacer.session)
    }

    fn view(&self) -> SessionView {
        self.read(echo_sub_session::State::view)
    }

    pub(crate) fn idle_deadline(&self) -> Option<Instant> {
        self.view().idle_deadline.map(Instant::from_std)
    }

    pub(crate) fn run_ceiling(&self) -> Option<Instant> {
        self.read(echo_sub_session::State::run_ceiling).map(Instant::from_std)
    }

    pub(crate) fn response_due(&self) -> Option<Instant> {
        self.read(echo_sub_session::State::response_due).map(Instant::from_std)
    }

    pub(crate) fn follow_up(&self) -> FollowUp {
        self.read(echo_sub_session::State::follow_up)
    }

    pub(crate) fn deadline_reached(&self, timer: SessionTimer, due: Instant) -> bool {
        self.dispatch(SessionChange::Deadline {
            timer,
            due: due.into_std(),
        })
        .outcome
            == Some(Ok(SessionOutcome::DeadlineReached(true)))
    }

    pub(crate) fn set_response_due(&self, due: Instant) {
        self.dispatch(SessionChange::ResponseDue(due.into_std()));
    }

    pub(crate) fn admit_response(&self, due: Instant) -> Result<(), Error> {
        self.apply(SessionChange::AdmitResponse(due.into_std()))
    }

    pub(crate) fn replacement_deadline(&self) -> Result<Instant, Error> {
        self.read(|state| state.replacement_deadline(Instant::now().into_std()))
            .map(Instant::from_std)
            .map_err(map_error)
    }

    pub(crate) fn reply_settled(&self) {
        self.dispatch(SessionChange::ReplySettled);
    }

    pub(crate) fn unanswered(&self) {
        self.dispatch(SessionChange::Unanswered);
    }

    pub(crate) fn listen_after_reply(&self, timeout: Duration) -> Result<(), Error> {
        self.apply(SessionChange::Listen(timeout))
    }

    pub(crate) fn resume(&self) -> Result<(), Error> {
        self.apply(SessionChange::Resume)
    }

    pub(crate) fn extend_idle(&self, deadline: Instant) {
        self.dispatch(SessionChange::ExtendIdle(deadline.into_std()));
    }

    pub(crate) fn begin_close(&self, mouth_drained: bool) -> Result<(), Error> {
        let commands = self.dispatch(SessionChange::BeginClose { mouth_drained });
        match commands.effect {
            Some(SessionCommand::EndSession { session }) if session == self.handle => Ok(()),
            _ => Err(session_error(commands.outcome)),
        }
    }

    pub(crate) fn finish_close(&self, mouth_drained: bool) -> Result<(), Error> {
        self.apply(SessionChange::FinishClose { mouth_drained })
    }

    pub(crate) fn played(&self, stopped: bool) -> Result<bool, Error> {
        match self
            .dispatch(SessionChange::ReplyPlayed {
                output_stopped: stopped,
            })
            .outcome
        {
            Some(Ok(SessionOutcome::Played(played))) => Ok(played),
            outcome => Err(session_error(outcome)),
        }
    }

    pub(crate) fn microphone_started(&self) {
        self.dispatch(SessionChange::CaptureStarted);
    }

    pub(crate) fn microphone_ended(&self) {
        self.dispatch(SessionChange::CaptureEnded);
    }

    pub(crate) fn reply_started(&self) {
        self.dispatch(SessionChange::ReplyStarted);
    }

    pub(crate) fn cut(&self) {
        self.dispatch(SessionChange::Cut);
    }

    pub(crate) fn cut_settled(&self) {
        self.dispatch(SessionChange::CutSettled);
    }

    pub(crate) fn cut_played(&self) {
        self.dispatch(SessionChange::CutPlayed);
    }

    pub(crate) fn closing(&self) -> bool {
        self.view().closing
    }

    pub(crate) fn speaking(&self) -> bool {
        self.view().speaking
    }

    pub(crate) fn microphone_finished(&self) -> bool {
        self.view().microphone_finished
    }

    pub(crate) fn accepts_audio(&self) -> bool {
        self.view().accepts_reply_audio
    }

    pub(crate) fn cutting(&self) -> bool {
        self.read(echo_sub_session::State::cutting)
    }

    pub(crate) fn retired(&self) -> bool {
        self.read(echo_sub_session::State::retired)
    }

    pub(crate) fn has_played(&self) -> bool {
        self.read(echo_sub_session::State::has_played)
    }

    pub(crate) fn pending_cut_receipts(&self) -> usize {
        self.read(echo_sub_session::State::pending_cut_receipts)
    }

    pub(crate) fn complete(&self, stopped: bool, mouth_drained: bool) -> bool {
        self.read(|state| state.complete(stopped, mouth_drained))
    }

    pub(crate) fn ready_to_close(&self, stopped: bool, mouth_drained: bool) -> bool {
        self.read(|state| state.ready_to_close(stopped, mouth_drained))
    }
}

impl Drop for SessionPort {
    fn drop(&mut self) {
        let mut state = self.world.state();
        let session = self.handle;
        let out = self
            .world
            .session_step(&mut state, SessionEvent::RunReleased { session });
        if out.effect == Some(SessionCommand::StopCapture { session }) {
            state.runs.remove(session.index, session.generation);
        }
    }
}

fn session_error(outcome: Option<Result<SessionOutcome, SessionError>>) -> Error {
    match outcome {
        Some(Err(error)) => map_error(error),
        _ => Error::Control,
    }
}

fn map_error(error: SessionError) -> Error {
    match error {
        SessionError::Input => Error::Input,
        SessionError::Control => Error::Control,
        SessionError::Deadline => Error::Deadline,
        SessionError::Playback => Error::Wyoming,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn session_slots_release_clears_quiet_deadline() {
        let world = Controller::new();
        let session = world.clone().admit(Instant::now(), None).unwrap();
        let window = Duration::from_secs(15);
        session.reply_started();
        session.played(true).unwrap();
        session.listen_after_reply(window).unwrap();
        let due = session.idle_deadline().unwrap();
        assert_eq!(
            session.view().next_deadline,
            Some((due.into_std(), SessionTimer::Listen))
        );
        assert!(!session.deadline_reached(SessionTimer::Listen, due));
        tokio::time::advance(window).await;
        assert!(session.deadline_reached(SessionTimer::Listen, due));
        session.begin_close(true).unwrap();
        assert_eq!(session.begin_close(true), Err(Error::Wyoming));
        assert_eq!(session.finish_close(true), Err(Error::Wyoming));
        session.microphone_ended();
        session.finish_close(true).unwrap();
        drop(session);

        let view = world.state().world.pacer.session.view();
        assert_eq!(view.session, None);
        assert_eq!(view.next_deadline, None);
    }

    #[test]
    fn session_slots_reuse_run_index_without_reopening_stale_run() {
        let world = Controller::new();
        let first = world.clone().admit(Instant::now(), None).unwrap();
        let stale = first.handle;
        assert!(matches!(world.clone().admit(Instant::now(), None), Err(Error::Control)));
        drop(first);

        let second = world.clone().admit(Instant::now(), None).unwrap();
        assert_eq!(second.handle.index, stale.index);
        assert_ne!(second.handle.generation, stale.generation);
        let mut state = world.state();
        let result = world.session_step(
            &mut state,
            SessionEvent::Run {
                session: stale,
                change: SessionChange::ReplyStarted,
            },
        );
        assert_eq!(result.outcome, Some(Err(SessionError::Control)));
        let view = state.world.pacer.session.view();
        assert_eq!(view.session, Some(second.handle));
        assert!(!view.speaking);
    }
}
