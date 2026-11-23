//! The user-speech latch and the run's structure-only trace marks.
//!
//! A user speech start latches "the user is speaking" until their transcript
//! or end of speech clears it. The pinned V3 session forwards no
//! speech-stopped, so a speech start that never produces transcript text
//! expires after `voice.speech_start_expiry_seconds` (owner, 2026-11-21) and
//! the run's no-answer, idle and listen deadlines apply again. Each end of
//! the latch is also the end of a user turn for the barge-in (`Floor`).
use crate::bridge_runtime::bridge_pacing::BargeTrigger;
use crate::bridge_voice::{Error, Notice, Source, UserSpeech};
use echo_core_api::FollowUp;
use serde_json::{Value, json};
use std::future::Future;
use tokio::time::{Duration, Instant};

/// `voice.speech_start_expiry_seconds` outside a configured scope.
pub(crate) const SPEECH_START_EXPIRY: Duration = Duration::from_secs(20);

/// The run's configured `[voice]` knobs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Knobs {
    /// `voice.speech_start_expiry_seconds`.
    pub expiry: Duration,
    /// `voice.local_barge_cut`: the bridge holds and cuts a reply the user
    /// talks over. Off, the model's own yield is the only barge-in.
    pub local_cut: bool,
}

impl Default for Knobs {
    fn default() -> Self {
        Self {
            expiry: SPEECH_START_EXPIRY,
            local_cut: true,
        }
    }
}

tokio::task_local! {
    static KNOBS: Knobs;
}

/// Run voice work under the configured knobs.
pub(crate) async fn scope<T>(knobs: Knobs, work: impl Future<Output = T>) -> T {
    KNOBS.scope(knobs, work).await
}

fn knobs() -> Knobs {
    KNOBS.try_with(|knobs| *knobs).unwrap_or_default()
}

pub(super) fn expiry() -> Duration {
    knobs().expiry
}

pub(in crate::bridge_voice) fn local_cut() -> bool {
    knobs().local_cut
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
    /// Speech after a settled reply went unanswered and its `window` lapsed:
    /// `listen` resumed when a speech start expired untranscribed, or
    /// `no_answer` after the user's end of speech. The run ends unfailed.
    Lapsed(&'static str),
    /// A barge-in armed by `trigger` cut the reply: the user `turn`'s one cut.
    BargeCut { turn: u32, trigger: BargeTrigger },
    /// The `count`-th sign of user speech over a reply in a `turn` that has
    /// had its cut: passed, so the reply (a backchannel) plays on.
    BargePassed {
        turn: u32,
        trigger: BargeTrigger,
        count: u32,
    },
    /// The user `turn` that cut ended (`cleared`, `expired`, or `ended`
    /// before its cut): the next turn may cut again.
    BargeRearmed { turn: u32, reason: &'static str },
    /// `voice.local_barge_cut` is off: the `turn`'s first sign over a reply
    /// was left to the model's own yield.
    BargeLeft { turn: u32, trigger: BargeTrigger },
}

impl Mark {
    #[must_use]
    pub fn event(&self) -> &'static str {
        match self {
            Self::Latched { .. } => "speech.latched",
            Self::Capped(_) => "run.capped",
            Self::Lapsed(_) => "run.lapsed",
            Self::BargeCut { .. } => "barge.cut",
            Self::BargePassed { .. } => "barge.suppressed_in_turn",
            Self::BargeRearmed { .. } => "barge.rearmed",
            Self::BargeLeft { .. } => "barge.model_only",
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
            Self::Lapsed(window) => json!({ "window": window }),
            Self::BargeCut { turn, trigger } | Self::BargeLeft { turn, trigger } => {
                json!({ "turn": turn, "trigger": trigger.name() })
            }
            Self::BargePassed { turn, trigger, count } => {
                json!({ "turn": turn, "trigger": trigger.name(), "count": count })
            }
            Self::BargeRearmed { turn, reason } => json!({ "turn": turn, "reason": reason }),
        }
    }
}

/// The open speech latch and the marks the run has not reported yet.
#[derive(Default)]
pub struct Marks {
    since: Option<Instant>,
    heard: Option<Instant>,
    transcribed: bool,
    /// The wait for an answer is the listen window resumed at an expiry.
    resumed: bool,
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

    /// Speech after a settled reply went unanswered: its window lapsed.
    pub fn lapsed(&mut self) {
        let window = if self.resumed { "listen" } else { "no_answer" };
        self.pending.push(Mark::Lapsed(window));
    }

    /// A new reply opened with its own no-answer window.
    pub(in crate::bridge_voice) fn reopened(&mut self) {
        self.resumed = false;
    }

    pub(in crate::bridge_voice) fn push(&mut self, mark: Mark) {
        self.pending.push(mark);
    }
}

impl Source {
    /// A speech start, or a partial user transcript: the user is speaking.
    /// Each renews the latch's expiry.
    pub fn user_active(&mut self, transcribed: bool) {
        let now = Instant::now();
        self.user_speech = UserSpeech::Active;
        self.marks.resumed = false;
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
            let resumed = (reason == "expired").then(|| self.resumed_listen(due)).flatten();
            self.marks.resumed = resumed.is_some();
            self.controller.set_response_due(resumed.unwrap_or(due));
        }
        self.marks.release(reason);
        self.turn_ended(reason);
    }

    /// A speech start after a settled reply expired with no transcript and no
    /// end of speech: no user turn was taken, so no answer is owed. The listen
    /// window resumes from the expiry, as from a settled reply, and a granted
    /// hold still holds; the no-answer window `due` stays the outer bound.
    /// None while anything answers: delegated work or a reply being spoken.
    fn resumed_listen(&self, due: Instant) -> Option<Instant> {
        if self.controller.follow_up() != FollowUp::Replied
            || self.delegation.waiting()
            || self.controller.speaking()
            || self.reply.voiced()
        {
            return None;
        }
        let window = Instant::now().checked_add(self.session.listen()?)?;
        let window = self.session.granted().map_or(window, |granted| window.max(granted));
        Some(window.min(due))
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
