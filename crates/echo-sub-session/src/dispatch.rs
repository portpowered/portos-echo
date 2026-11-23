//! Session timing projections of the provider-owned delegated turns.
use std::time::Instant;

/// The assistant utterance in progress, by its role in a delegated turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Utterance {
    #[default]
    None,
    Filler,
    /// A filler whose delegated turn completed before its done.
    Settled,
    Answer,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Answer {
    #[default]
    None,
    Pending,
    Delivered,
}

#[derive(Debug, Default)]
pub(super) struct Dispatch {
    turns: usize,
    armed: bool,
    utterance: Utterance,
    held: bool,
    answer: Answer,
    /// When the last running turn completed.
    resolved: Option<Instant>,
}

impl Dispatch {
    pub(super) fn started(&mut self) {
        self.turns += 1;
        self.answer = Answer::Pending;
        self.resolved = None;
        if self.utterance == Utterance::Answer {
            self.utterance = Utterance::Filler;
        } else {
            self.armed = true;
        }
    }

    pub(super) fn completed(&mut self, now: Instant) {
        self.turns = self.turns.saturating_sub(1);
        if self.turns == 0 {
            self.armed = false;
            self.resolved = Some(now);
            if self.utterance == Utterance::Filler {
                self.utterance = Utterance::Settled;
            }
        }
    }

    /// A delegated turn has started and not yet completed.
    #[must_use]
    pub(super) fn in_flight(&self) -> bool {
        self.turns > 0
    }

    pub(super) fn waiting(&self) -> bool {
        self.in_flight() || self.held || self.answer == Answer::Pending
    }

    /// When every turn had resolved, while their answer is still owed.
    #[must_use]
    pub(super) fn resolved(&self) -> Option<Instant> {
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

    pub(super) fn spoke(&mut self) {
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
    pub(super) fn answered(&mut self) -> bool {
        let utterance = std::mem::take(&mut self.utterance);
        self.held = matches!(utterance, Utterance::Filler | Utterance::Settled);
        if !self.held {
            self.answer = Answer::Delivered;
        }
        !self.held
    }

    /// No answer follows the completed turns: true when the reply finishes
    /// now. A filler still being spoken finishes at its own done instead.
    pub(super) fn released(&mut self) -> bool {
        let finish = std::mem::take(&mut self.held) || self.utterance == Utterance::None;
        self.utterance = if finish { Utterance::None } else { Utterance::Answer };
        finish
    }

    /// An applied end owes nothing delegated any more. An utterance still
    /// being spoken as the end's farewell is the answer: its own done ends the
    /// reply, so a delegated turn completing first cannot cut its tail.
    pub(super) fn ended(&mut self, farewell: bool) {
        let speaking = farewell && self.utterance != Utterance::None;
        *self = Self::default();
        if speaking {
            self.utterance = Utterance::Answer;
        }
    }

    /// A filler done is holding the reply open for a delegated answer.
    #[must_use]
    pub(super) fn held(&self) -> bool {
        self.held
    }
}
