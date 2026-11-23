use crate::run::Run;
use echo_core_api::{FollowUp, SessionKnobs, SessionMark as Mark, SessionOutcome, SessionTimer, SessionWait};
use std::time::{Duration, Instant};

/// Leave enough time for a resolved delegated turn's answer to start.
pub const HANDBACK_GRACE: Duration = Duration::from_secs(20);

impl Run {
    pub(crate) fn next_deadline(&self, idle: Option<Instant>, knobs: SessionKnobs) -> Option<(Instant, SessionTimer)> {
        if self.closing() {
            return None;
        }
        let (due, wait) = idle.map_or_else(
            || {
                self.reply_deadline(
                    self.playback.played_due,
                    self.playback.finished,
                    self.playback.voiced,
                    knobs,
                )
            },
            |idle| self.listen_deadline(idle, knobs),
        );
        let timer = match wait {
            SessionWait::Listen => SessionTimer::Listen,
            SessionWait::Response => SessionTimer::Response,
            SessionWait::Ceiling => SessionTimer::RunCeiling,
            SessionWait::Playback => SessionTimer::Playback,
        };
        [
            self.turn
                .latch_expiry(knobs)
                .map(|due| (due, SessionTimer::SpeechLatch)),
            due.map(|due| (due, timer)),
            self.turn
                .barge_due(self.epoch, self.playback_observed, self.playback.finished)
                .map(|due| (due, SessionTimer::Barge)),
        ]
        .into_iter()
        .flatten()
        .min_by_key(|(due, _)| *due)
    }

    pub(crate) fn reply_wait(
        &self,
        played: Option<Instant>,
        finished: bool,
        voiced: bool,
        knobs: SessionKnobs,
    ) -> (Option<Instant>, SessionWait) {
        let (due, wait) = self.reply_deadline(played, finished, voiced, knobs);
        (earliest(due, self.turn.latch_expiry(knobs)), wait)
    }

    fn reply_deadline(
        &self,
        played: Option<Instant>,
        finished: bool,
        voiced: bool,
        knobs: SessionKnobs,
    ) -> (Option<Instant>, SessionWait) {
        if played.is_some() || finished {
            return (played, SessionWait::Playback);
        }
        let dispatch = &self.turn.dispatch;
        if dispatch.waiting() {
            let answering = voiced || (self.speaking() && !dispatch.held());
            return match dispatch.resolved().filter(|_| !self.turn.speaking() && !answering) {
                Some(resolved) => (Some(self.owed(resolved)), SessionWait::Response),
                None => (self.run_cap(knobs).filter(|_| !answering), SessionWait::Ceiling),
            };
        }
        if self.turn.speaking() {
            return (self.run_cap(knobs), SessionWait::Ceiling);
        }
        (Some(self.response_due), SessionWait::Response)
    }

    pub(crate) fn listening_wait(&self, idle: Instant, knobs: SessionKnobs) -> (Option<Instant>, SessionWait) {
        let (due, wait) = self.listen_deadline(idle, knobs);
        (earliest(due, self.turn.latch_expiry(knobs)), wait)
    }

    fn listen_deadline(&self, idle: Instant, knobs: SessionKnobs) -> (Option<Instant>, SessionWait) {
        let dispatch = &self.turn.dispatch;
        if dispatch.waiting() {
            match dispatch.resolved().filter(|_| !self.turn.speaking()) {
                Some(resolved) => (Some(self.owed(resolved)), SessionWait::Response),
                None => (self.run_cap(knobs), SessionWait::Ceiling),
            }
        } else {
            (
                Some(self.run_cap(knobs).map_or(idle, |cap| idle.min(cap))),
                SessionWait::Listen,
            )
        }
    }

    /// Speech defers the run cap by at most one latch expiry.
    fn run_cap(&self, knobs: SessionKnobs) -> Option<Instant> {
        let cap = self.run_ceiling?;
        Some(if self.turn.speaking() {
            cap.checked_add(knobs.expiry).unwrap_or(cap)
        } else {
            cap
        })
    }

    fn owed(&self, resolved: Instant) -> Instant {
        resolved
            .checked_add(HANDBACK_GRACE)
            .map_or(self.response_due, |grace| grace.max(self.response_due))
    }

    pub(crate) fn wait_expired(
        &mut self,
        (wait, due, listening): (SessionWait, Option<Instant>, bool),
        voiced: bool,
        idle: Option<Instant>,
        knobs: SessionKnobs,
        now: Instant,
    ) -> SessionOutcome {
        if self.turn.latch_expiry(knobs).is_some_and(|expiry| now >= expiry) {
            self.user_quiet(now, "expired", voiced);
            return SessionOutcome::Applied;
        }
        let timer = match wait {
            SessionWait::Listen => Some(SessionTimer::Listen),
            SessionWait::Response => Some(SessionTimer::Response),
            SessionWait::Ceiling => Some(SessionTimer::RunCeiling),
            SessionWait::Playback => None,
        };
        if let Some(timer) = timer
            && !due.is_some_and(|due| self.deadline_reached(timer, due, now, idle))
        {
            return SessionOutcome::Applied;
        }
        if listening {
            if self.run_ceiling.is_some_and(|ceiling| now >= ceiling) {
                self.turn.marks.push(Mark::Capped("listen"));
            }
        } else {
            if wait == SessionWait::Ceiling {
                self.turn.marks.push(Mark::Capped("reply"));
            }
            if wait == SessionWait::Response
                && self.follow_up == FollowUp::Replied
                && !self.speaking()
                && !self.turn.dispatch.waiting()
            {
                self.follow_up = FollowUp::Unanswered;
                let window = if self.turn.resumed_listen {
                    "listen"
                } else {
                    "no_answer"
                };
                self.turn.marks.push(Mark::Lapsed(window));
                return SessionOutcome::Unanswered;
            }
        }
        SessionOutcome::WaitExpired
    }
}

pub(crate) fn earliest(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}
