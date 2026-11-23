use echo_core_api::{
    FollowUp, SessionError as Error, SessionHandle, SessionOutcome, SessionTimer, SessionToolAction,
    SessionToolDecision, SessionToolEvent, SessionToolRefusal, ToolCallHandle,
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

/// A cut acknowledgment belongs to the retired playback, even when its ordered
/// device event arrives after the replacement playback has started.
#[derive(Debug)]
pub(crate) struct Run {
    pub(crate) handle: SessionHandle,
    pub(crate) epoch: Instant,
    pub(crate) run_ceiling: Option<Instant>,
    pub(crate) response_due: Instant,
    pub(crate) follow_up: FollowUp,
    state: Phase,
    played: bool,
    /// The device closed a replacement playback that nothing was written to.
    unanswered: bool,
    cut_receipts: usize,
    microphone_finished: bool,
    response_window: Option<Duration>,
    idle_deadline: Option<Instant>,
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
    pub(crate) fn new(handle: SessionHandle, epoch: Instant, run_ceiling: Option<Instant>, now: Instant) -> Self {
        Self {
            handle,
            epoch,
            run_ceiling,
            response_due: now,
            follow_up: FollowUp::First,
            state: Phase::Listening,
            played: false,
            unanswered: false,
            cut_receipts: 0,
            microphone_finished: false,
            response_window: None,
            idle_deadline: None,
            tools: None,
            active_tool: None,
            granted: None,
            ending: false,
        }
    }

    /// Start idle only after both speaker acknowledgments and mouth drain.
    pub(crate) fn listen_after_reply(&mut self, timeout: Duration, now: Instant) -> Result<(), Error> {
        if timeout.is_zero() || !self.ready_to_close(true, true) {
            return Err(Error::Input);
        }
        let deadline = now.checked_add(timeout).ok_or(Error::Input)?;
        self.idle_deadline = Some(self.granted.map_or(deadline, |grant| deadline.max(grant)));
        self.state = Phase::Listening;
        Ok(())
    }

    pub(crate) fn idle_deadline(&self) -> Option<Instant> {
        self.idle_deadline
    }

    /// A granted deadline can only lengthen an armed listen interval.
    fn extend_idle(&mut self, deadline: Instant) {
        if let Some(current) = &mut self.idle_deadline {
            *current = (*current).max(deadline);
        }
    }

    pub(crate) fn closing(&self) -> bool {
        matches!(self.state, Phase::Closing | Phase::Closed)
    }

    /// Ordered speech or assistant activity admits the next response window.
    pub(crate) fn resume(&mut self) -> Result<(), Error> {
        if matches!(self.state, Phase::Closing | Phase::Closed) {
            return Err(Error::Control);
        }
        self.idle_deadline = None;
        self.played = false;
        self.unanswered = false;
        Ok(())
    }

    /// Commit the run to explicit end before performing device IO. A failed
    /// or cancelled end cannot reopen reply admission or issue another end.
    pub(crate) fn begin_close(&mut self, mouth_drained: bool) -> Result<(), Error> {
        if !self.ready_to_close(true, mouth_drained) {
            return Err(Error::Playback);
        }
        self.state = Phase::Closing;
        self.idle_deadline = None;
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
        self.idle_deadline.is_none()
            && !matches!(
                self.state,
                Phase::Cutting | Phase::Retired | Phase::Closing | Phase::Closed
            )
    }

    pub(crate) fn reply_started(&mut self) {
        self.state = Phase::Replying;
    }

    pub(crate) fn cut(&mut self) {
        self.state = Phase::Cutting;
    }

    pub(crate) fn cut_settled(&mut self) {
        self.state = Phase::Retired;
        self.cut_receipts += usize::from(!self.played);
        self.played = false;
    }

    /// A persistent run settles a cut reply as over: nothing more of it will
    /// play, and the next reply opens its own playback.
    pub(crate) fn cut_played(&mut self) {
        (self.played, self.unanswered) = (true, true);
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
        self.unanswered = self.state == Phase::Retired;
        if !stopped && !self.unanswered {
            return Err(Error::Playback);
        }
        self.played = true;
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
        self.played
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
        !self.cutting() && self.played && (stopped || self.unanswered) && mouth_drained
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

    pub(crate) fn renew_idle(&mut self, now: Instant) {
        if let Some(deadline) = self.tools.as_ref().and_then(|tools| now.checked_add(tools.listen)) {
            self.extend_idle(deadline);
        }
    }

    pub(crate) fn projected_listen(&self, now: Instant) -> Option<Instant> {
        let window = now.checked_add(self.tools.as_ref()?.listen)?;
        Some(self.granted.map_or(window, |grant| window.max(grant)))
    }

    pub(crate) fn tool_event(&mut self, event: SessionToolEvent, now: Instant) -> Result<SessionOutcome, Error> {
        match event {
            SessionToolEvent::Started { tool, action } if self.active_tool.is_none() => {
                self.active_tool = Some(tool);
                Ok(SessionOutcome::Tool(self.decide_tool(tool, action, now)))
            }
            SessionToolEvent::Ended { tool } if self.active_tool == Some(tool) => {
                self.active_tool = None;
                Ok(SessionOutcome::Applied)
            }
            _ => Err(Error::Control),
        }
    }

    fn decide_tool(&mut self, tool: ToolCallHandle, action: SessionToolAction, now: Instant) -> SessionToolDecision {
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
                let current = self.idle_deadline.or_else(|| now.checked_add(window.listen));
                let base = [current, self.granted].into_iter().flatten().fold(now, Instant::max);
                let cap = window.opened.checked_add(window.max).unwrap_or(base);
                let wanted = base.checked_add(Duration::from_secs(seconds));
                let capped = wanted.is_none_or(|wanted| wanted > cap);
                let deadline = wanted.map_or(cap, |wanted| wanted.min(cap));
                let deadline_ms =
                    u64::try_from(deadline.saturating_duration_since(window.opened).as_millis()).unwrap_or(u64::MAX);
                self.granted = Some(deadline);
                self.extend_idle(deadline);
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
    pub(crate) fn next_deadline(&self) -> Option<(Instant, SessionTimer)> {
        if self.closing() {
            return None;
        }
        let reply = self
            .idle_deadline
            .map_or((self.response_due, SessionTimer::Response), |idle| {
                (idle, SessionTimer::Listen)
            });
        Some(
            self.run_ceiling
                .filter(|cap| *cap < reply.0)
                .map_or(reply, |cap| (cap, SessionTimer::RunCeiling)),
        )
    }

    pub(crate) fn deadline_reached(&self, timer: SessionTimer, due: Instant, now: Instant) -> bool {
        match timer {
            SessionTimer::Listen => self.idle_deadline.is_some_and(|idle| now >= due && due <= idle),
            SessionTimer::Response => now >= due && due >= self.response_due,
            SessionTimer::RunCeiling => self.run_ceiling.is_some_and(|cap| now >= due && due >= cap),
        }
    }
}
