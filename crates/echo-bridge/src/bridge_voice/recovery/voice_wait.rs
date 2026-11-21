//! Delegated response timing belongs to the admitted Wyoming run.
use super::{Error, Source, UserSpeech, voice_source::FollowUp};
use tokio::time::{Duration, Instant};

/// How long a resolved hand-off's answer may take to start. The voice model
/// begins a handed-back answer within a few seconds; calls on the Echo showed
/// more than six. Twenty covers that three times over, and a hand-back that
/// never arrives still ends well inside one no-answer window.
pub(super) const HANDBACK_GRACE: Duration = Duration::from_secs(20);

/// The reply's deadline and what its expiry means. The no-answer window runs
/// from the user's last end of speech, never while they are still speaking,
/// and again from the last reply audio written while the reply is voiced: a
/// reply of any length plays to its end, and one that stalls still ends.
pub(super) fn deadline(source: &Source, played: Option<Instant>) -> (Option<Instant>, Error) {
    if played.is_some() {
        return (played, Error::Deadline);
    }
    if source.reply.finished() {
        // The answer has arrived. Its queued media is still owned by the
        // playback operation until the Played timer takes over.
        return (None, Error::Deadline);
    }
    let speaking = source.user_speech == UserSpeech::Active;
    if source.delegation.waiting() {
        let active = speaking || source.mouth.voiced() || (source.controller.speaking() && !source.delegation.held());
        return match source.delegation.resolved().filter(|_| !active) {
            Some(resolved) => (Some(owed(source, resolved)), Error::NoResponse),
            None => (source.run_ceiling.filter(|_| !active), Error::SessionCeiling),
        };
    }
    ((!speaking).then_some(source.answer_due), Error::NoResponse)
}

/// A resolved hand-off holds the run for its answer: to the end of the
/// no-answer window, and at least the grace from its resolution.
fn owed(source: &Source, resolved: Instant) -> Instant {
    resolved
        .checked_add(HANDBACK_GRACE)
        .map_or(source.answer_due, |grace| grace.max(source.answer_due))
}

/// The wait between replies and what its expiry returns: a running turn has
/// the run ceiling, a resolved one its answer's grace, the rest the listen window.
pub(super) fn listening(source: &Source, idle: Instant) -> (Option<Instant>, Result<bool, Error>) {
    if !source.delegation.waiting() {
        return (Some(idle), Ok(false));
    }
    let quiet = source.user_speech == UserSpeech::Quiet;
    match source.delegation.resolved() {
        Some(resolved) => (quiet.then(|| owed(source, resolved)), Err(Error::NoResponse)),
        None => (source.run_ceiling.filter(|_| quiet), Err(Error::SessionCeiling)),
    }
}

/// The reply's deadline passed. Speech nobody answered after an earlier reply
/// is not a failed run: the reply closes empty and the run ends the usual way.
pub(super) fn expired(source: &mut Source, error: Error) -> Result<(), Error> {
    if error != Error::NoResponse
        || source.follow_up != FollowUp::Replied
        || source.controller.speaking()
        || source.delegation.waiting()
    {
        return Err(error);
    }
    source.follow_up = FollowUp::Unanswered;
    source.reply.finish()
}

/// Delegated Codex turns and the assistant utterance they bracket. Each
/// delegated turn has at most one filler ("one sec"): the utterance in progress
/// when the turn starts, or else the first one begun while it runs. A filler's
/// done holds the reply open; any later utterance is the answer, even when it
/// begins before turn/completed arrives. A filler still speaking after its turn
/// completed has run on into the answer, so its done ends the reply. The order
/// of the filler's done, the answer's deltas and turn/completed varies by call.
/// The assistant utterance in progress, by its role in a delegated turn.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Utterance {
    #[default]
    None,
    Filler,
    /// A filler whose delegated turn completed before its done.
    Settled,
    Answer,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Answer {
    #[default]
    None,
    Pending,
    Delivered,
}

#[derive(Default)]
pub struct Delegation {
    turns: usize,
    armed: bool,
    utterance: Utterance,
    held: bool,
    answer: Answer,
    /// When the last running turn completed.
    resolved: Option<Instant>,
}

impl Delegation {
    pub fn started(&mut self) {
        self.turns += 1;
        self.answer = Answer::Pending;
        self.resolved = None;
        if self.utterance == Utterance::Answer {
            self.utterance = Utterance::Filler;
        } else {
            self.armed = true;
        }
    }

    pub fn completed(&mut self) {
        self.turns = self.turns.saturating_sub(1);
        if self.turns == 0 {
            self.armed = false;
            self.resolved = Some(Instant::now());
            if self.utterance == Utterance::Filler {
                self.utterance = Utterance::Settled;
            }
        }
    }

    /// A delegated turn has started and not yet completed.
    #[must_use]
    pub fn in_flight(&self) -> bool {
        self.turns > 0
    }

    pub fn waiting(&self) -> bool {
        self.in_flight() || self.held || self.answer == Answer::Pending
    }

    /// When every turn had resolved, while their answer is still owed.
    #[must_use]
    pub fn resolved(&self) -> Option<Instant> {
        self.resolved.filter(|_| !self.in_flight() && self.waiting())
    }

    /// The next reply starts clean but still owes each running turn its
    /// completion; an utterance after the reset is an answer, not a filler.
    pub(super) fn carried(&self) -> Self {
        Self {
            turns: self.turns,
            answer: self.answer,
            resolved: self.resolved,
            ..Self::default()
        }
    }

    pub fn spoke(&mut self) {
        self.utterance = match self.utterance {
            Utterance::None if self.armed && self.turns > 0 => {
                self.armed = false;
                Utterance::Filler
            }
            Utterance::None | Utterance::Settled => Utterance::Answer,
            utterance => utterance,
        };
    }

    /// True when this assistant done ends the reply rather than a filler.
    pub fn answered(&mut self) -> bool {
        let utterance = std::mem::take(&mut self.utterance);
        self.held = matches!(utterance, Utterance::Filler | Utterance::Settled);
        if !self.held {
            self.answer = Answer::Delivered;
        }
        !self.held
    }

    /// No answer follows the completed turns: true when the reply finishes
    /// now. A filler still being spoken finishes at its own done instead.
    pub fn released(&mut self) -> bool {
        let finish = std::mem::take(&mut self.held) || self.utterance == Utterance::None;
        self.utterance = if finish { Utterance::None } else { Utterance::Answer };
        finish
    }

    /// A filler done is holding the reply open for a delegated answer.
    #[must_use]
    pub fn held(&self) -> bool {
        self.held
    }
}

#[cfg(test)]
mod tests {
    use super::{super::Options, *};
    use std::time::Duration;

    #[test]
    fn delegated_completion_does_not_reopen_a_finished_reply() {
        let mut delegation = Delegation::default();
        delegation.started();
        delegation.spoke();
        assert!(!delegation.answered()); // Filler.
        delegation.spoke();
        assert!(delegation.answered());
        delegation.completed();
        assert!(!delegation.waiting());
    }

    #[tokio::test(start_paused = true)]
    async fn delegated_wait_keeps_its_admission_ceiling_through_speech_and_playback() {
        let opened = Instant::now();
        let options = Options {
            session_max: Duration::from_secs(90),
            response_deadline: opened + Duration::from_secs(60),
            played_timeout: Duration::from_secs(5),
            listen_timeout: None,
            reply_lead_ms: 200,
            muted: false,
        };
        let mut source = Source::new(200, false).unwrap();
        let ceiling = opened + options.session_max;
        source.run_ceiling = Some(ceiling);
        source.answer_due = options.response_deadline;
        assert_eq!(
            deadline(&source, None),
            (Some(options.response_deadline), Error::NoResponse)
        );
        source.delegation.started();
        tokio::time::advance(Duration::from_secs(61)).await;
        assert_eq!(deadline(&source, None), (Some(ceiling), Error::SessionCeiling));
        source.delegation.completed();
        source.delegation.started();
        source.user_speech = UserSpeech::Active;
        tokio::time::advance(Duration::from_secs(30)).await;
        assert_eq!(deadline(&source, None).0, None);
        source.user_speech = UserSpeech::Quiet;
        source.spoke();
        assert_eq!(deadline(&source, None).0, None);
        assert!(!source.delegation.answered());
        let audible = [2000_i16.to_le_bytes(); 480].concat();
        source.mouth.wrote(&audible, Instant::now());
        assert_eq!(deadline(&source, None).0, None);
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(source.mouth.poll(Instant::now()), Some(true));
        assert_eq!(source.mouth.poll(Instant::now()), Some(false));
        assert_eq!(deadline(&source, None), (Some(ceiling), Error::SessionCeiling));
        source.delegation.completed();
        source.spoke();
        assert!(source.delegation.answered());
        source.reply.finish().unwrap();
        assert_eq!(deadline(&source, None), (None, Error::Deadline));
        let played = Instant::now() + options.played_timeout;
        assert_eq!(deadline(&source, Some(played)), (Some(played), Error::Deadline));
    }

    /// A run admitted at `opened` with the Echo's own windows: a 15 s listen
    /// window and the default `voice.session_max_seconds` of 600 s.
    fn admitted(opened: Instant) -> (Source, Instant) {
        let mut source = Source::new(200, false).unwrap();
        let ceiling = opened + Duration::from_secs(600);
        source.run_ceiling = Some(ceiling);
        source.answer_due = opened + Duration::from_secs(15);
        (source, ceiling)
    }

    #[tokio::test(start_paused = true)]
    async fn a_long_delegated_turn_outlives_the_no_answer_and_listen_windows() {
        let (mut source, ceiling) = admitted(Instant::now());
        let idle = Instant::now() + Duration::from_secs(15);
        source.delegation.started();
        // Two minutes of thinking and tool calls, far past both windows.
        tokio::time::advance(Duration::from_secs(120)).await;
        assert_eq!(deadline(&source, None), (Some(ceiling), Error::SessionCeiling));
        assert_eq!(listening(&source, idle), (Some(ceiling), Err(Error::SessionCeiling)));
        // Its completion re-arms the wait: the answer's grace, then no answer.
        source.delegation.completed();
        let owed = Instant::now() + HANDBACK_GRACE;
        assert_eq!(deadline(&source, None), (Some(owed), Error::NoResponse));
        assert_eq!(listening(&source, idle), (Some(owed), Err(Error::NoResponse)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_session_with_nothing_in_progress_still_ends_at_its_windows() {
        let (source, _) = admitted(Instant::now());
        let idle = Instant::now() + Duration::from_secs(15);
        assert!(!source.delegation.waiting());
        assert_eq!(deadline(&source, None), (Some(source.answer_due), Error::NoResponse));
        assert_eq!(listening(&source, idle), (Some(idle), Ok(false)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_delegated_turn_ends_at_the_run_ceiling() {
        let opened = Instant::now();
        let (mut source, ceiling) = admitted(opened);
        source.delegation.started();
        tokio::time::advance(Duration::from_secs(599)).await;
        let idle = Instant::now() + Duration::from_secs(15);
        assert_eq!(deadline(&source, None), (Some(ceiling), Error::SessionCeiling));
        assert_eq!(listening(&source, idle), (Some(ceiling), Err(Error::SessionCeiling)));
        assert_eq!(ceiling.duration_since(opened), Duration::from_secs(600));
    }
}
