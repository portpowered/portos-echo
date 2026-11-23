use crate::turn::Turn;
use echo_core_api::{
    FollowUp, SessionCommand, SessionCommands, SessionError as Error, SessionHandle, SessionOutcome, SessionPlayback,
    SessionTimer, SessionToolAction, SessionToolDecision, SessionToolEvent, SessionToolRefusal, ToolCallHandle,
};
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
enum Phase {
    Listening,
    Replying,
    Cutting,
    Retired,
    Closing,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Receipt {
    Waiting,
    Played,
    /// Replacement playback ended without writing any audio.
    Retired {
        acknowledged: bool,
    },
}

/// A cut acknowledgment belongs to the retired playback, even when its ordered
/// device event arrives after the replacement playback has started.
#[derive(Debug)]
pub(crate) struct Run {
    pub(crate) handle: SessionHandle,
    pub(crate) epoch: Instant,
    pub(crate) call_opening: bool,
    pub(crate) ceiling: Option<Instant>,
    pub(crate) response_due: Instant,
    pub(crate) follow_up: FollowUp,
    pub(crate) turn: Turn,
    pub(crate) playback: SessionPlayback,
    pub(crate) playback_observed: Instant,
    state: Phase,
    receipt: Receipt,
    cut_receipts: usize,
    microphone_finished: bool,
    response_window: Option<Duration>,
    tools: Option<ToolWindow>,
    active_tool: Option<ToolCallHandle>,
    granted: Option<Instant>,
    ending: bool,
}

#[derive(Debug)]
struct ToolWindow {
    opened: Instant,
    listen: Duration,
    max: Duration,
}

impl Run {
    pub(crate) fn new(
        handle: SessionHandle,
        epoch: Instant,
        ceiling: Instant,
        now: Instant,
        response_due: Instant,
        reuse_call: bool,
    ) -> Self {
        Self {
            handle,
            epoch,
            call_opening: !reuse_call,
            ceiling: Some(ceiling),
            response_due,
            follow_up: FollowUp::First,
            turn: Turn::default(),
            playback: SessionPlayback::default(),
            playback_observed: now,
            state: Phase::Listening,
            receipt: Receipt::Waiting,
            cut_receipts: 0,
            microphone_finished: false,
            response_window: reuse_call
                .then(|| {
                    response_due
                        .checked_duration_since(now)
                        .filter(|window| !window.is_zero())
                })
                .flatten(),
            tools: None,
            active_tool: None,
            granted: None,
            ending: false,
        }
    }

    /// Start idle only after both speaker acknowledgments and mouth drain.
    pub(crate) fn listen_after_reply(&mut self, timeout: Duration, now: Instant) -> Result<Instant, Error> {
        if timeout.is_zero() || !self.ready_to_close(true, true) {
            return Err(Error::Input);
        }
        let deadline = now.checked_add(timeout).ok_or(Error::Input)?;
        self.state = Phase::Listening;
        Ok(self.granted.map_or(deadline, |grant| deadline.max(grant)))
    }

    pub(crate) fn closing(&self) -> bool {
        matches!(self.state, Phase::Closing | Phase::Closed)
    }

    /// Ordered speech or assistant activity admits the next response window.
    pub(crate) fn resume(&mut self) -> Result<(), Error> {
        if matches!(self.state, Phase::Closing | Phase::Closed) {
            return Err(Error::Control);
        }
        self.receipt = Receipt::Waiting;
        Ok(())
    }

    /// Commit the run to explicit end before performing device IO. A failed
    /// or cancelled end cannot reopen reply admission or issue another end.
    pub(crate) fn begin_close(&mut self, mouth_drained: bool) -> Result<(), Error> {
        if !self.ready_to_close(true, mouth_drained) {
            return Err(Error::Playback);
        }
        self.state = Phase::Closing;
        Ok(())
    }

    /// Called only after the explicit end write and capture drain succeed.
    pub(crate) fn finish_close(&mut self, mouth_drained: bool) -> Result<(), Error> {
        if self.state != Phase::Closing || !self.playback_settled(true, mouth_drained) || !self.microphone_finished {
            return Err(Error::Playback);
        }
        self.state = Phase::Closed;
        Ok(())
    }

    /// Retain the admitted response budget independently of any one reply.
    pub(crate) fn admit_response(&mut self, deadline: Instant, now: Instant) -> Result<(), Error> {
        let window = deadline
            .checked_duration_since(now)
            .filter(|window| !window.is_zero())
            .ok_or(Error::Deadline)?;
        self.response_window = Some(window);
        Ok(())
    }

    /// A settled cut starts a fresh response window for the replacement.
    pub(crate) fn replacement_deadline(&self, now: Instant) -> Result<Instant, Error> {
        now.checked_add(self.response_window.ok_or(Error::Input)?)
            .ok_or(Error::Input)
    }

    pub(crate) fn speaking(&self) -> bool {
        self.state == Phase::Replying
    }

    pub(crate) fn cutting(&self) -> bool {
        self.state == Phase::Cutting
    }

    pub(crate) fn retired(&self) -> bool {
        self.state == Phase::Retired
    }

    pub(crate) fn accepts_audio(&self) -> bool {
        !self.call_opening
            && !matches!(
                self.state,
                Phase::Cutting | Phase::Retired | Phase::Closing | Phase::Closed
            )
    }

    pub(crate) fn reply_started(&mut self) {
        self.state = Phase::Replying;
        self.turn.stale = false;
    }

    pub(crate) fn cut(&mut self) {
        self.state = Phase::Cutting;
        self.turn.stale = true;
    }

    pub(crate) fn cut_settled(&mut self) {
        self.state = Phase::Retired;
        self.cut_receipts += usize::from(!self.has_played());
        self.receipt = match self.receipt {
            Receipt::Retired { .. } => Receipt::Retired { acknowledged: false },
            _ => Receipt::Waiting,
        };
    }

    /// A persistent run settles a cut reply as over: nothing more of it will
    /// play, and the next reply opens its own playback.
    pub(crate) fn cut_played(&mut self) {
        self.receipt = Receipt::Retired { acknowledged: true };
    }

    /// False consumes a delayed cut receipt; true acknowledges this reply.
    /// After a barge-in the model may say nothing more. The device then ends
    /// the replacement playback itself, and the cut reply settles as played:
    /// the run listens on instead of failing.
    pub(crate) fn played(&mut self, stopped: bool) -> Result<bool, Error> {
        if self.cut_receipts > 0 {
            self.cut_receipts -= 1;
            return Ok(false);
        }
        if self.closing() {
            return Err(Error::Playback);
        }
        if self.state == Phase::Retired {
            self.receipt = Receipt::Retired { acknowledged: true };
        } else if stopped || self.has_played() {
            self.receipt = Receipt::Played;
        } else {
            self.receipt = Receipt::Waiting;
        }
        if !stopped && self.state != Phase::Retired {
            return Err(Error::Playback);
        }
        Ok(true)
    }

    pub(crate) fn microphone_ended(&mut self) {
        self.microphone_finished = true;
    }

    pub(crate) fn microphone_started(&mut self) {
        self.microphone_finished = false;
    }

    pub(crate) fn microphone_finished(&self) -> bool {
        self.microphone_finished
    }

    pub(crate) fn has_played(&self) -> bool {
        matches!(self.receipt, Receipt::Played | Receipt::Retired { acknowledged: true })
    }

    pub(crate) fn pending_cut_receipts(&self) -> usize {
        self.cut_receipts
    }

    pub(crate) fn complete(&self, stopped: bool, mouth_drained: bool) -> bool {
        self.state == Phase::Closed && self.playback_settled(stopped, mouth_drained) && self.microphone_finished
    }

    /// Playback can settle while duplex capture is still open. Never wait for
    /// the EOF caused by the end command before deciding to issue that command.
    pub(crate) fn ready_to_close(&self, stopped: bool, mouth_drained: bool) -> bool {
        !matches!(self.state, Phase::Closing | Phase::Closed) && self.playback_settled(stopped, mouth_drained)
    }

    fn playback_settled(&self, stopped: bool, mouth_drained: bool) -> bool {
        !self.cutting()
            && self.has_played()
            && (stopped || matches!(self.receipt, Receipt::Retired { .. }))
            && mouth_drained
    }
}

impl Run {
    pub(crate) fn configure_tools(&mut self, listen: Option<Duration>, max: Duration, now: Instant) {
        self.tools = listen.map(|listen| ToolWindow {
            opened: now,
            listen,
            max,
        });
    }

    pub(crate) fn ending(&self) -> bool {
        self.ending
    }

    pub(crate) fn request_end(&mut self) {
        self.ending = true;
    }

    pub(crate) fn granted(&self) -> Option<Instant> {
        self.granted
    }

    pub(crate) fn renew_idle(&self, idle: &mut Option<Instant>, now: Instant) {
        if let Some(deadline) = self.tools.as_ref().and_then(|tools| now.checked_add(tools.listen)) {
            extend_idle(idle, deadline);
        }
    }

    pub(crate) fn projected_listen(&self, now: Instant) -> Option<Instant> {
        let window = now.checked_add(self.tools.as_ref()?.listen)?;
        Some(self.granted.map_or(window, |grant| window.max(grant)))
    }

    pub(crate) fn tool_event(
        &mut self,
        event: SessionToolEvent,
        idle: &mut Option<Instant>,
        now: Instant,
        out: &mut SessionCommands,
    ) -> Result<SessionOutcome, Error> {
        match event {
            SessionToolEvent::Started { tool, action, .. } if self.active_tool.is_none() && !self.call_opening => {
                self.active_tool = Some(tool);
                out.effects[0] = Some(SessionCommand::Tool(self.decide_tool(tool, action, idle, now)));
                Ok(SessionOutcome::Applied)
            }
            SessionToolEvent::Ended { tool, .. } if self.active_tool == Some(tool) => {
                self.active_tool = None;
                Ok(SessionOutcome::Applied)
            }
            _ => Err(Error::Control),
        }
    }

    fn decide_tool(
        &mut self,
        tool: ToolCallHandle,
        action: SessionToolAction,
        idle: &mut Option<Instant>,
        now: Instant,
    ) -> SessionToolDecision {
        let refuse = |reason| SessionToolDecision::Refused { tool, reason };
        let Some(window) = &self.tools else {
            return refuse(SessionToolRefusal::Inactive);
        };
        if self.closing() {
            return refuse(SessionToolRefusal::Closing);
        }
        match action {
            SessionToolAction::Invalid => refuse(SessionToolRefusal::InvalidArguments),
            SessionToolAction::End => {
                let duplicate = self.ending();
                self.request_end();
                SessionToolDecision::Ending { tool, duplicate }
            }
            SessionToolAction::Extend { .. } if self.ending() => refuse(SessionToolRefusal::Closing),
            SessionToolAction::Extend { seconds } => {
                let current = idle.or_else(|| now.checked_add(window.listen));
                let base = [current, self.granted].into_iter().flatten().fold(now, Instant::max);
                let cap = window.opened.checked_add(window.max).unwrap_or(base);
                let wanted = base.checked_add(Duration::from_secs(seconds));
                let capped = wanted.is_none_or(|wanted| wanted > cap);
                let deadline = wanted.map_or(cap, |wanted| wanted.min(cap));
                let deadline_ms =
                    u64::try_from(deadline.saturating_duration_since(window.opened).as_millis()).unwrap_or(u64::MAX);
                self.granted = Some(deadline);
                extend_idle(idle, deadline);
                SessionToolDecision::Extended {
                    tool,
                    deadline_ms,
                    capped,
                }
            }
        }
    }
}

impl Run {
    pub(crate) fn deadline_reached(
        &self,
        timer: SessionTimer,
        due: Instant,
        now: Instant,
        idle: Option<Instant>,
    ) -> bool {
        match timer {
            SessionTimer::Listen => idle.is_some_and(|idle| now >= due && due <= idle),
            SessionTimer::Response => now >= due && due >= self.response_due,
            SessionTimer::RunCeiling => self.ceiling.is_some_and(|cap| now >= due && due >= cap),
            SessionTimer::Playback => self
                .playback
                .played_due
                .is_some_and(|played| now >= played && due == played),
            SessionTimer::SpeechLatch | SessionTimer::Barge => false,
        }
    }
}

/// A granted deadline can only lengthen an armed listen interval.
fn extend_idle(idle: &mut Option<Instant>, deadline: Instant) {
    if let Some(current) = idle {
        *current = (*current).max(deadline);
    }
}
