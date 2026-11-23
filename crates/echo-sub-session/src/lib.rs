//! Pure lifecycle of the controller's admitted voice run.
use echo_core_api::{
    Broken, CommandKind, Commands, Event, EventKind, FollowUp, Now, SessionChange, SessionCommand, SessionCommands,
    SessionError, SessionEvent, SessionHandle, SessionOutcome, SessionView, Subsystem,
};
use std::time::{Duration, Instant};

mod run;
use run::Run;

#[derive(Debug, Default)]
pub struct State {
    run: Option<Run>,
}

pub struct Session;

impl Subsystem for Session {
    type State = State;
    const INPUTS: &'static [EventKind] = &[EventKind::Session];
    const OUTPUTS: &'static [CommandKind] = &[
        CommandKind::StartCapture,
        CommandKind::StopCapture,
        CommandKind::EndSession,
        CommandKind::OpenCall,
        CommandKind::CloseCall,
    ];

    fn on_event(state: &mut State, event: &mut Event, now: Now, out: &mut Commands) {
        let Event::Session(event) = *event else { return };
        let out = out.session();
        out.outcome = Some(match event {
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
            SessionEvent::Run { session, change } => state.active(session).and_then(|run| {
                let time = run.epoch.checked_add(now.monotonic).ok_or(SessionError::Deadline)?;
                run.apply(change, time, out)
            }),
            SessionEvent::RunReleased { session } if state.is_active(session) => {
                state.run = None;
                out.effect = Some(SessionCommand::StopCapture { session });
                Ok(SessionOutcome::Applied)
            }
            _ => Err(SessionError::Control),
        });
    }

    fn next_deadline(state: &State) -> Option<Duration> {
        let run = state.run.as_ref()?;
        let (deadline, _) = run.next_deadline()?;
        Some(deadline.saturating_duration_since(run.epoch))
    }

    fn check(state: &State) -> Result<(), Broken> {
        if let Some(run) = &state.run
            && run.closing()
            && run.idle_deadline().is_some()
        {
            return Err(Broken {
                field: "session.idle_deadline",
                expected: "absent while closing",
                actual: "present",
            });
        }
        Ok(())
    }
}

impl State {
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

    fn active(&mut self, session: SessionHandle) -> Result<&mut Run, SessionError> {
        self.run
            .as_mut()
            .filter(|run| run.handle == session)
            .ok_or(SessionError::Control)
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
            accepts_reply_audio: self.run.as_ref().is_some_and(Run::accepts_audio),
            idle_deadline: self.run.as_ref().and_then(Run::idle_deadline),
            next_deadline: self.run.as_ref().and_then(Run::next_deadline),
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
    pub fn response_due(&self) -> Option<Instant> {
        self.run.as_ref().map(|run| run.response_due)
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
        now: Instant,
        out: &mut SessionCommands,
    ) -> Result<SessionOutcome, SessionError> {
        match change {
            SessionChange::CaptureEnded => self.microphone_ended(),
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
            SessionChange::Resume => self.resume()?,
            SessionChange::Listen(timeout) => self.listen_after_reply(timeout, now)?,
            SessionChange::ExtendIdle(due) => self.extend_idle(due),
            SessionChange::Cut => self.cut(),
            SessionChange::CutSettled => self.cut_settled(),
            SessionChange::CutPlayed => self.cut_played(),
            SessionChange::Unanswered => self.follow_up = FollowUp::Unanswered,
            SessionChange::Deadline { timer, due } => {
                return Ok(SessionOutcome::DeadlineReached(self.deadline_reached(timer, due, now)));
            }
            SessionChange::BeginClose { mouth_drained } => {
                self.begin_close(mouth_drained)?;
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
