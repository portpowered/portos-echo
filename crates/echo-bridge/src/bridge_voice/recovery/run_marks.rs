//! The user-speech latch and the run's structure-only trace marks.
//!
//! A user speech start latches "the user is speaking" until their transcript
//! or end of speech clears it. The pinned V3 session forwards no
//! speech-stopped, so a speech start that never produces transcript text
//! expires after `voice.speech_start_expiry_seconds` (owner, 2026-11-21) and
//! the run's no-answer, idle and listen deadlines apply again.
use crate::bridge_voice::{Error, Notice, Source, UserSpeech};
use serde_json::{Value, json};
use std::future::Future;
use tokio::time::{Duration, Instant};

/// `voice.speech_start_expiry_seconds` outside a configured scope.
pub(crate) const SPEECH_START_EXPIRY: Duration = Duration::from_secs(20);

tokio::task_local! {
    static EXPIRY: Duration;
}

/// Run voice work under the configured speech-start expiry.
pub(crate) async fn scope<T>(expiry: Duration, work: impl Future<Output = T>) -> T {
    EXPIRY.scope(expiry, work).await
}

pub(super) fn expiry() -> Duration {
    EXPIRY.try_with(|expiry| *expiry).unwrap_or(SPEECH_START_EXPIRY)
}

/// One structure-only trace fact: no transcript text, ever.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// A latched speech start ended: `cleared` by the user's transcript or end
    /// of speech, `expired` with no transcript text, or open at `run_end`.
    /// `transcribed` says whether any partial transcript arrived meanwhile.
    Latched {
        held: Duration,
        reason: &'static str,
        transcribed: bool,
    },
    /// The run cap ended a `reply` or a `listen`.
    Capped(&'static str),
}

impl Mark {
    #[must_use]
    pub fn event(&self) -> &'static str {
        match self {
            Self::Latched { .. } => "speech.latched",
            Self::Capped(_) => "run.capped",
        }
    }

    #[must_use]
    pub fn fields(&self) -> Value {
        match *self {
            Self::Latched {
                held,
                reason,
                transcribed,
            } => json!({
                "held_ms": u64::try_from(held.as_millis()).unwrap_or(u64::MAX),
                "reason": reason,
                "transcribed": transcribed,
            }),
            Self::Capped(during) => json!({ "during": during }),
        }
    }
}

/// The open speech latch and the marks the run has not reported yet.
#[derive(Default)]
pub struct Marks {
    since: Option<Instant>,
    heard: Option<Instant>,
    transcribed: bool,
    pending: Vec<Mark>,
}

impl Marks {
    fn release(&mut self, reason: &'static str) {
        if let Some(since) = self.since.take() {
            let transcribed = std::mem::take(&mut self.transcribed);
            self.pending.push(Mark::Latched {
                held: since.elapsed(),
                reason,
                transcribed,
            });
        }
        self.heard = None;
    }

    /// The run cap ended a `reply` or a `listen`.
    pub fn capped(&mut self, during: &'static str) {
        self.pending.push(Mark::Capped(during));
    }
}

impl Source {
    /// A speech start, or a partial user transcript: the user is speaking.
    /// Each renews the latch's expiry.
    pub fn user_active(&mut self, transcribed: bool) {
        let now = Instant::now();
        self.user_speech = UserSpeech::Active;
        self.marks.since.get_or_insert(now);
        self.marks.heard = Some(now);
        self.marks.transcribed |= transcribed;
    }

    /// The user stopped speaking: the Live session's end of speech, their
    /// whole transcript or the end of capture. The no-answer window restarts.
    pub fn user_quiet(&mut self) {
        self.quiet("cleared");
    }

    fn quiet(&mut self, reason: &'static str) {
        self.user_speech = UserSpeech::Quiet;
        if let Ok(due) = self.controller.replacement_deadline() {
            self.answer_due = due;
        }
        self.marks.release(reason);
    }

    /// When a latched speech start with no transcript since stops counting
    /// as speech. None while the user is quiet.
    #[must_use]
    pub fn latch_expiry(&self) -> Option<Instant> {
        if self.user_speech != UserSpeech::Active {
            return None;
        }
        self.marks.heard.and_then(|heard| heard.checked_add(expiry()))
    }

    /// Clear an expired latch as an end of speech would. True if it expired.
    pub fn expire_latch(&mut self) -> bool {
        let due = self.latch_expiry().is_some_and(|expiry| Instant::now() >= expiry);
        if due {
            self.quiet("expired");
        }
        due
    }
}

/// Hand the run's pending marks to its observer, oldest first.
pub(in crate::bridge_voice) fn report(
    source: &mut Source,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    for mark in std::mem::take(&mut source.marks.pending) {
        observe(Notice::Mark(mark))?;
    }
    Ok(())
}

/// The run ended: a latch still open ends with it.
pub(in crate::bridge_voice) fn run_ended(
    source: &mut Source,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    source.marks.release("run_end");
    report(source, observe)
}
