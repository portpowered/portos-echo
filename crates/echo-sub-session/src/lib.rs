//! Pure lifecycle of the controller's admitted voice run.
use echo_core_api::{
    Broken, CommandKind, Commands, Event, EventKind, FollowUp, Now, SessionChange, SessionCommand, SessionCommands,
    SessionError, SessionEvent, SessionHandle, SessionKnobs, SessionMark, SessionOutcome, SessionPlayback,
    SessionTurnEvent, SessionView, SessionWait, Subsystem,
};
use std::time::{Duration, Instant};

mod barge;
mod dispatch;
mod run;
mod turn;
mod wait;
pub use barge::{BARGE_QUIET, BARGE_WINDOW};
use run::Run;
pub use wait::HANDBACK_GRACE;

#[derive(Debug, Default)]
pub struct State {
    run: Option<Run>,
    idle_deadline: Option<Instant>,
    knobs: SessionKnobs,
}

pub struct Session;

impl Subsystem for Session {
    type State = State;
    const INPUTS: &'static [EventKind] = &[EventKind::Session];
    const OUTPUTS: &'static [CommandKind] = &[
        CommandKind::StartCapture,
        CommandKind::StopCapture,
        CommandKind::EndSession,
        CommandKind::CutReply,
        CommandKind::OpenCall,
        CommandKind::CloseCall,
    ];

    fn on_event(state: &mut State, event: &mut Event, now: Now, out: &mut Commands) {
        let Event::Session(event) = event else { return };
        let out = out.session();
        out.outcome = Some(match *event {
            SessionEvent::Configure(knobs) if state.run.is_none() => {
                state.knobs = knobs;
                Ok(SessionOutcome::Applied)
            }
            SessionEvent::CallAdmitted if state.run.is_none() => {
                out.effect = Some(SessionCommand::OpenCall);
                Ok(SessionOutcome::Applied)
            }
            SessionEvent::CallCloseDue if state.run.is_none() => {
                out.effect = Some(SessionCommand::CloseCall);
                Ok(SessionOutcome::Applied)
            }
            SessionEvent::Wake {
                session,
                epoch,
                opened,
                max,
            } if state.run.is_none() => state.wake(session, epoch, opened, max, now.monotonic, out),
            SessionEvent::Run { .. } | SessionEvent::Turn { .. } | SessionEvent::Tool { .. } => {
                state.step_run(event, now.monotonic, out)
            }
            SessionEvent::RunReleased { session } if state.is_active(session) => {
                state.run = None;
                state.idle_deadline = None;
                out.effect = Some(SessionCommand::StopCapture { session });
                Ok(SessionOutcome::Applied)
            }
            _ => Err(SessionError::Control),
        });
    }

    fn next_deadline(state: &State) -> Option<Duration> {
        let run = state.run.as_ref()?;
        let (deadline, _) = run.next_deadline(state.idle_deadline, state.knobs)?;
        Some(deadline.saturating_duration_since(run.epoch))
    }

    fn check(state: &State) -> Result<(), Broken> {
        if state.idle_deadline.is_some() {
            let expected = match &state.run {
                None => "absent without active run",
                Some(run) if run.closing() => "absent while closing",
                Some(_) => return Ok(()),
            };
            return Err(Broken {
                field: "session.idle_deadline",
                expected,
                actual: "present",
            });
        }
        Ok(())
    }
}

impl State {
    fn step_run(
        &mut self,
        event: &SessionEvent,
        offset: Duration,
        out: &mut SessionCommands,
    ) -> Result<SessionOutcome, SessionError> {
        let (SessionEvent::Run { session, .. }
        | SessionEvent::Turn { session, .. }
        | SessionEvent::Tool { session, .. }) = event
        else {
            return Err(SessionError::Control);
        };
        let run = self
            .run
            .as_mut()
            .filter(|run| run.handle == *session)
            .ok_or(SessionError::Control)?;
        let now = run.epoch.checked_add(offset).ok_or(SessionError::Deadline)?;
        let idle = &mut self.idle_deadline;
        match event {
            SessionEvent::Run { change, .. } => run.apply(*change, idle, self.knobs, now, out),
            SessionEvent::Tool { change, .. } => run.tool_event(*change, idle, now),
            SessionEvent::Turn { change, .. }
                if !run.closing()
                    || matches!(
                        change,
                        SessionTurnEvent::RunEnded | SessionTurnEvent::SpeechStopped { .. }
                    ) =>
            {
                Ok(run.turn_event(change, idle, self.knobs, now, out))
            }
            _ => Err(SessionError::Control),
        }
    }

    #[must_use]
    pub fn ending(&self) -> bool {
        self.run.as_ref().is_some_and(Run::ending)
    }

    #[must_use]
    pub fn granted(&self) -> Option<Instant> {
        self.run.as_ref().and_then(Run::granted)
    }

    fn wake(
        &mut self,
        session: SessionHandle,
        epoch: Instant,
        opened: Instant,
        max: Option<Duration>,
        offset: Duration,
        out: &mut SessionCommands,
    ) -> Result<SessionOutcome, SessionError> {
        let now = epoch.checked_add(offset).ok_or(SessionError::Deadline)?;
        let ceiling = max
            .map(|max| opened.checked_add(max).ok_or(SessionError::Deadline))
            .transpose()?;
        self.run = Some(Run::new(session, epoch, ceiling, now));
        out.effect = Some(SessionCommand::StartCapture { session });
        Ok(SessionOutcome::Applied)
    }

    fn is_active(&self, session: SessionHandle) -> bool {
        self.run.as_ref().is_some_and(|run| run.handle == session)
    }

    #[must_use]
    pub fn view(&self) -> SessionView {
        SessionView {
            session: self.run.as_ref().map(|run| run.handle),
            closing: self.run.as_ref().is_some_and(Run::closing),
            speaking: self.run.as_ref().is_some_and(Run::speaking),
            microphone_finished: self.run.as_ref().is_some_and(Run::microphone_finished),
            accepts_reply_audio: self.idle_deadline.is_none() && self.run.as_ref().is_some_and(Run::accepts_audio),
            idle_deadline: self.idle_deadline,
            next_deadline: self
                .run
                .as_ref()
                .and_then(|run| run.next_deadline(self.idle_deadline, self.knobs)),
        }
    }

    #[must_use]
    pub fn cutting(&self) -> bool {
        self.run.as_ref().is_some_and(Run::cutting)
    }

    #[must_use]
    pub fn retired(&self) -> bool {
        self.run.as_ref().is_some_and(Run::retired)
    }

    #[must_use]
    pub fn stale(&self) -> bool {
        self.run.as_ref().is_some_and(|run| run.turn.stale || run.retired())
    }

    #[must_use]
    pub fn dispatch_in_flight(&self) -> bool {
        self.run.as_ref().is_some_and(|run| run.turn.dispatch.in_flight())
    }

    #[must_use]
    pub fn dispatch_waiting(&self) -> bool {
        self.run.as_ref().is_some_and(|run| run.turn.dispatch.waiting())
    }

    #[must_use]
    pub fn dispatch_held(&self) -> bool {
        self.run.as_ref().is_some_and(|run| run.turn.dispatch.held())
    }

    pub fn take_marks(&mut self) -> Vec<SessionMark> {
        self.run
            .as_mut()
            .map_or_else(Vec::new, |run| std::mem::take(&mut run.turn.marks))
    }

    #[must_use]
    pub fn barge_pending(&self) -> bool {
        self.run.as_ref().is_some_and(|run| run.turn.barge_pending())
    }

    #[must_use]
    pub fn barge_due(&self, now: Instant, finished: bool) -> Option<Instant> {
        let run = self.run.as_ref()?;
        run.turn.barge_due(run.epoch, now, finished)
    }

    #[must_use]
    pub fn reply_wait(&self, played: Option<Instant>, finished: bool, voiced: bool) -> (Option<Instant>, SessionWait) {
        self.run.as_ref().map_or((None, SessionWait::Playback), |run| {
            run.reply_wait(played, finished, voiced, self.knobs)
        })
    }

    #[must_use]
    pub fn playback_matches(&self, playback: SessionPlayback) -> bool {
        self.run.as_ref().is_some_and(|run| run.playback == playback)
    }

    #[must_use]
    pub fn listening_wait(&self, idle: Instant) -> (Option<Instant>, SessionWait) {
        self.run
            .as_ref()
            .map_or((None, SessionWait::Listen), |run| run.listening_wait(idle, self.knobs))
    }

    #[must_use]
    pub fn has_played(&self) -> bool {
        self.run.as_ref().is_some_and(Run::has_played)
    }

    #[must_use]
    pub fn pending_cut_receipts(&self) -> usize {
        self.run.as_ref().map_or(0, Run::pending_cut_receipts)
    }

    #[must_use]
    pub fn complete(&self, stopped: bool, mouth_drained: bool) -> bool {
        self.run
            .as_ref()
            .is_some_and(|run| run.complete(stopped, mouth_drained))
    }

    #[must_use]
    pub fn ready_to_close(&self, stopped: bool, mouth_drained: bool) -> bool {
        self.run
            .as_ref()
            .is_some_and(|run| run.ready_to_close(stopped, mouth_drained))
    }

    /// # Errors
    /// The run has no admitted response window, or the resulting deadline overflows.
    pub fn replacement_deadline(&self, now: Instant) -> Result<Instant, SessionError> {
        self.run
            .as_ref()
            .ok_or(SessionError::Control)?
            .replacement_deadline(now)
    }

    #[must_use]
    pub fn run_ceiling(&self) -> Option<Instant> {
        self.run.as_ref().and_then(|run| run.run_ceiling)
    }

    #[must_use]
    pub fn follow_up(&self) -> FollowUp {
        self.run.as_ref().map_or(FollowUp::First, |run| run.follow_up)
    }
}

impl Run {
    fn apply(
        &mut self,
        change: SessionChange,
        idle: &mut Option<Instant>,
        knobs: SessionKnobs,
        now: Instant,
        out: &mut SessionCommands,
    ) -> Result<SessionOutcome, SessionError> {
        match change {
            SessionChange::CaptureEnded => {
                self.microphone_ended();
            }
            SessionChange::FinishClose { mouth_drained } => self.finish_close(mouth_drained)?,
            _ if self.closing() => return Err(closing_error(change)),
            SessionChange::AdmitResponse(due) => {
                self.admit_response(due, now)?;
                self.response_due = due;
            }
            SessionChange::ResponseDue(due) => self.response_due = due,
            SessionChange::CaptureStarted => self.microphone_started(),
            SessionChange::ReplyStarted => self.reply_started(),
            SessionChange::ReplyPlayed { output_stopped } => {
                return self.played(output_stopped).map(SessionOutcome::Played);
            }
            SessionChange::ReplySettled => self.follow_up = FollowUp::Replied,
            SessionChange::Resume { preserve_dispatch } => {
                self.resume()?;
                *idle = None;
                self.turn.resume(preserve_dispatch);
                self.playback = SessionPlayback::default();
                self.playback_observed = now;
            }
            SessionChange::Listen(timeout) => *idle = Some(self.listen_after_reply(timeout, now)?),
            SessionChange::ConfigureTools { listen, max } => self.configure_tools(listen, max, now),
            SessionChange::RenewIdle => self.renew_idle(idle, now),
            SessionChange::RequestEnd => self.request_end(),
            SessionChange::Cut => self.cut(),
            SessionChange::CutSettled => self.cut_settled(),
            SessionChange::CutPlayed => self.cut_played(),
            SessionChange::Playback(playback) => {
                self.playback = playback;
                self.playback_observed = now;
            }
            SessionChange::Deadline { timer, due } => {
                return Ok(SessionOutcome::DeadlineReached(
                    self.deadline_reached(timer, due, now, *idle),
                ));
            }
            SessionChange::WaitExpired {
                wait,
                due,
                voiced,
                listening,
            } => {
                return Ok(self.wait_expired((wait, due, listening), voiced, *idle, knobs, now));
            }
            SessionChange::BeginClose { mouth_drained } => {
                self.begin_close(mouth_drained)?;
                *idle = None;
                out.effect = Some(SessionCommand::EndSession { session: self.handle });
            }
        }
        Ok(SessionOutcome::Applied)
    }
}

fn closing_error(change: SessionChange) -> SessionError {
    match change {
        SessionChange::Listen(_) => SessionError::Input,
        SessionChange::BeginClose { .. } | SessionChange::ReplyPlayed { .. } => SessionError::Playback,
        _ => SessionError::Control,
    }
}
