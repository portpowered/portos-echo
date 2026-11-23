use super::{Controller, State, call, pacer};
use crate::Error;
#[cfg(test)]
use echo_core_api::SessionTimer;
use echo_core_api::{
    CallCommands, CallEvent, Commands, Event, FollowUp, SessionChange, SessionCommand, SessionCommands, SessionError,
    SessionEvent, SessionHandle, SessionOutcome, SessionToolAction, SessionToolDecision, SessionToolEvent, SessionView,
    UplinkEvent,
};
use std::{fmt, future::Future, sync::Arc, time::Duration};
use tokio::time::Instant;

mod turn;

/// The service's lease on the active run. The slot is released on all exits,
/// including cancellation and a failed playback or device acknowledgement.
pub(crate) struct SessionPort {
    world: Arc<Controller>,
    handle: SessionHandle,
    token: Arc<()>,
}

impl fmt::Debug for SessionPort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.world.state().world.pacer.session, formatter)
    }
}

impl Controller {
    fn session_step(&self, state: &mut State, event: SessionEvent) -> SessionCommands {
        self.session_envelope(state, &mut Event::Session(event))
    }

    fn session_envelope(&self, state: &mut State, event: &mut Event) -> SessionCommands {
        let mut out = Commands::default();
        state.world.step(event, &mut out);
        self.check_step(state);
        out.take_session()
    }

    fn admit(
        self: Arc<Self>,
        opened: Instant,
        listen: Option<Duration>,
        max: Duration,
        response_due: Instant,
        reuse_call: bool,
    ) -> Result<SessionPort, Error> {
        let mut state = self.state();
        let (index, generation) = state.runs.insert(()).ok_or(Error::Control)?;
        let session = SessionHandle { index, generation };
        let event = SessionEvent::Wake {
            session,
            opened: opened.into_std(),
            listen,
            max,
            response_due: response_due.into_std(),
            reuse_call,
        };
        let result = self.session_step(&mut state, event);
        if result.effects
            != [
                Some(SessionCommand::StartCapture { session }),
                (!reuse_call).then_some(SessionCommand::OpenCall { session }),
            ]
        {
            state.runs.remove(index, generation);
            return Err(session_error(result.outcome));
        }
        call::step(
            &self,
            &mut state,
            CallEvent::Uplink {
                run: session,
                event: UplinkEvent::CaptureOpened,
            },
        );
        drop(state);
        Ok(SessionPort {
            world: self,
            handle: session,
            token: Arc::new(()),
        })
    }

    #[cfg(test)]
    fn admit_fixture(self: Arc<Self>, opened: Instant, max: Option<Duration>) -> Result<SessionPort, Error> {
        self.admit(opened, None, max.unwrap_or(Duration::from_secs(600)), opened, true)
    }
}

/// Admit the run before opening its call. The voice loop replaces this initial
/// response deadline once negotiation has completed.
pub(crate) async fn open_call(
    opened: Instant,
    listen: Option<Duration>,
    max: Duration,
    response_timeout: Duration,
    work: impl Future<Output = Result<(), Error>>,
) -> Result<SessionPort, Error> {
    let response_due = opened.checked_add(response_timeout).ok_or(Error::Deadline)?;
    let run =
        pacer::current_controller()
            .unwrap_or_else(Controller::new)
            .admit(opened, listen, max, response_due, false)?;
    work.await?;
    run.apply_event(SessionEvent::CallOpened { session: run.handle })?;
    Ok(run)
}

pub(crate) async fn close_call(work: impl Future<Output = Result<(), Error>>) -> Result<(), Error> {
    let world = pacer::current_controller().unwrap_or_else(Controller::new);
    let out = world.session_step(&mut world.state(), SessionEvent::CallCloseDue);
    if out.effects[0] != Some(SessionCommand::CloseCall) {
        return Err(session_error(out.outcome));
    }
    work.await
}

impl SessionPort {
    #[cfg(test)]
    pub(crate) fn open(opened: Instant, max: Option<Duration>) -> Result<Self, Error> {
        pacer::current_controller()
            .unwrap_or_else(Controller::new)
            .admit_fixture(opened, max)
    }

    pub(crate) fn on_connected_call(
        opened: Instant,
        listen: Option<Duration>,
        max: Duration,
        response_due: Instant,
    ) -> Result<Self, Error> {
        pacer::current_controller()
            .unwrap_or_else(Controller::new)
            .admit(opened, listen, max, response_due, true)
    }

    fn dispatch(&self, change: SessionChange) -> SessionCommands {
        self.send(SessionEvent::Run {
            session: self.handle,
            change,
        })
    }

    fn send(&self, event: SessionEvent) -> SessionCommands {
        let mut state = self.world.state();
        let session = self.handle;
        if state.runs.get(session.index, session.generation).is_none() {
            return SessionCommands::default();
        }
        self.world.session_step(&mut state, event)
    }

    fn apply(&self, change: SessionChange) -> Result<(), Error> {
        self.apply_event(SessionEvent::Run {
            session: self.handle,
            change,
        })
    }

    fn apply_event(&self, event: SessionEvent) -> Result<(), Error> {
        match self.send(event).outcome {
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

    pub(crate) fn token(&self) -> &Arc<()> {
        &self.token
    }

    pub(crate) fn uplink(&self, event: UplinkEvent<'_>) -> CallCommands {
        call::step(
            &self.world,
            &mut self.world.state(),
            CallEvent::Uplink {
                run: self.handle,
                event,
            },
        )
    }

    pub(crate) fn uplink_pending(&self) -> bool {
        let view = self.world.state().world.pacer.call.view();
        view.uplink_run == Some(self.handle) && view.backlog_depth != 0
    }

    pub(crate) fn answer_tool(
        &self,
        key: String,
        action: SessionToolAction,
    ) -> Result<(SessionToolDecision, bool), Error> {
        let mut state = self.world.state();
        let session = self.handle;
        if state.runs.get(session.index, session.generation).is_none() {
            return Err(Error::Control);
        }
        if let Some(cached) = state.tools.replay(&key) {
            return Ok((cached, true));
        }
        let tool = state.tools.start(key).ok_or(Error::Control)?;
        let start = SessionEvent::Tool(SessionToolEvent::Started { session, tool, action });
        let mut commands = self.world.session_step(&mut state, start);
        let Some(SessionCommand::Tool(decision)) = commands.take_effect() else {
            state.tools.remove(tool);
            return Err(session_error(commands.outcome));
        };
        let end = SessionEvent::Tool(SessionToolEvent::Ended { session, tool });
        let outcome = self.world.session_step(&mut state, end).outcome;
        if outcome != Some(Ok(SessionOutcome::Applied)) {
            state.tools.remove(tool);
            return Err(session_error(outcome));
        }
        state.tools.retain(tool, decision).ok_or(Error::Control)?;
        Ok((decision, false))
    }

    pub(crate) fn configure_tools(&self, listen: Option<Duration>, max: Duration) -> Result<(), Error> {
        self.apply(SessionChange::ConfigureTools { listen, max })
    }

    pub(crate) fn ending(&self) -> bool {
        self.read(echo_sub_session::State::ending)
    }

    pub(crate) fn request_end(&self) {
        self.send(SessionEvent::ExplicitEnd { session: self.handle });
    }

    #[cfg(test)]
    pub(crate) fn granted(&self) -> Option<Instant> {
        self.read(echo_sub_session::State::granted).map(Instant::from_std)
    }

    #[cfg(test)]
    pub(crate) fn answered_count(&self) -> usize {
        self.world.state().tools.len()
    }

    pub(crate) fn idle_deadline(&self) -> Option<Instant> {
        self.view().idle_deadline.map(Instant::from_std)
    }

    pub(crate) fn run_ceiling(&self) -> Option<Instant> {
        self.read(echo_sub_session::State::run_ceiling).map(Instant::from_std)
    }

    pub(crate) fn follow_up(&self) -> FollowUp {
        self.read(echo_sub_session::State::follow_up)
    }

    #[cfg(test)]
    fn deadline_reached(&self, timer: SessionTimer) -> bool {
        self.send(SessionEvent::Deadline {
            session: self.handle,
            timer,
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
        self.send(SessionEvent::ReplySettled { session: self.handle });
    }

    pub(crate) fn listen_after_reply(&self, timeout: Duration) -> Result<(), Error> {
        self.apply(SessionChange::Listen(timeout))
    }

    pub(crate) fn resume(&self, preserve_dispatch: bool) -> Result<(), Error> {
        self.apply(SessionChange::Resume { preserve_dispatch })
    }

    pub(crate) fn renew_idle(&self) {
        self.dispatch(SessionChange::RenewIdle);
    }

    pub(crate) fn begin_close(&self, mouth_drained: bool) -> Result<(), Error> {
        let commands = self.dispatch(SessionChange::BeginClose { mouth_drained });
        if commands.effects[0] == Some(SessionCommand::EndSession { session: self.handle }) {
            Ok(())
        } else {
            Err(session_error(commands.outcome))
        }
    }

    pub(crate) fn finish_close(&self, mouth_drained: bool) -> Result<(), Error> {
        self.apply(SessionChange::FinishClose { mouth_drained })
    }

    pub(crate) fn played(&self, stopped: bool) -> Result<bool, Error> {
        match self
            .send(SessionEvent::ReplyPlayed {
                session: self.handle,
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
        self.send(SessionEvent::CaptureEnded { session: self.handle });
    }

    pub(crate) fn reply_started(&self) {
        self.send(SessionEvent::ReplyStarted { session: self.handle });
    }

    pub(crate) fn reply_ended(&self) -> SessionCommands {
        self.send(SessionEvent::ReplyEnded { session: self.handle })
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

    #[cfg(test)]
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

    #[cfg(test)]
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
        if out.effects[0] == Some(SessionCommand::StopCapture { session }) {
            call::step(
                &self.world,
                &mut state,
                CallEvent::Uplink {
                    run: session,
                    event: UplinkEvent::CaptureClosed,
                },
            );
            state.tools.clear();
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
mod tests;
