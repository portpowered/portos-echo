//! Delegated response timing belongs to the admitted Wyoming run.
use super::{Error, Options, Source, UserSpeech};
use tokio::time::Instant;

pub(super) fn deadline(source: &Source, options: &Options, played: Option<Instant>) -> (Option<Instant>, Error) {
    if played.is_some() {
        return (played, Error::Deadline);
    }
    if source.delegation.waiting() {
        let active = source.user_speech == UserSpeech::Active
            || source.mouth.voiced()
            || (source.controller.speaking() && !source.delegation.held());
        return (source.run_ceiling.filter(|_| !active), Error::SessionCeiling);
    }
    (Some(options.response_deadline), Error::NoResponse)
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
}

impl Delegation {
    pub fn started(&mut self) {
        self.turns += 1;
        self.answer = Answer::Pending;
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

    /// The next reply starts clean but still owes each running turn its
    /// completion; an utterance after the reset is an answer, not a filler.
    pub(super) fn carried(&self) -> Self {
        Self {
            turns: self.turns,
            answer: self.answer,
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
    use super::*;
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
        assert_eq!(
            deadline(&source, &options, None),
            (Some(options.response_deadline), Error::NoResponse)
        );
        source.delegation.started();
        tokio::time::advance(Duration::from_secs(61)).await;
        assert_eq!(
            deadline(&source, &options, None),
            (Some(ceiling), Error::SessionCeiling)
        );
        source.delegation.completed();
        source.delegation.started();
        source.user_speech = UserSpeech::Active;
        tokio::time::advance(Duration::from_secs(30)).await;
        assert_eq!(deadline(&source, &options, None).0, None);
        source.user_speech = UserSpeech::Quiet;
        source.spoke();
        assert_eq!(deadline(&source, &options, None).0, None);
        assert!(!source.delegation.answered());
        let audible = [2000_i16.to_le_bytes(); 480].concat();
        source.mouth.wrote(&audible, Instant::now());
        assert_eq!(deadline(&source, &options, None).0, None);
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(source.mouth.poll(Instant::now()), Some(true));
        assert_eq!(source.mouth.poll(Instant::now()), Some(false));
        assert_eq!(
            deadline(&source, &options, None),
            (Some(ceiling), Error::SessionCeiling)
        );
        let played = Instant::now() + options.played_timeout;
        assert_eq!(
            deadline(&source, &options, Some(played)),
            (Some(played), Error::Deadline)
        );
    }
}
