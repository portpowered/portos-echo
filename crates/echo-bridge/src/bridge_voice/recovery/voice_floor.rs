//! A barge-in: the Live session's sign of the user talking over a reply,
//! one cut per user turn (`Floor`), and the cut itself.
//!
//! On the pinned V3 route the model is full duplex and yields by itself:
//! Codex sends no turn detection and no cancel, and forwards no speech-start.
//! What the bridge adds is the hold and the flush of the audio it has queued
//! for the device. `voice.local_barge_cut` off leaves the yield to the model.
use super::{Error, Source, VoicePort};
use crate::bridge_runtime::bridge_pacing::{BargeTrigger, Cut, reply_lips};
use crate::bridge_voice::voice_wait::{Delegation, Mark, run_marks};
use tokio::time::Instant;

impl Source {
    /// The Live session's sign of user speech during a reply arms a barge-in:
    /// `said` is its transcript, a fragment or the `whole` turn. The reply is
    /// held from the device at once, but not cut: the model may talk on (`Barge`).
    /// A dispatch in flight is thinking, not a reply to talk over: user speech
    /// neither cancels the delegated turn nor cuts the speaker until it
    /// completes. The Live session still hears that speech. A user turn that
    /// has cut a reply holds and cuts nothing more until it ends (`Floor`).
    pub fn heard_user(&mut self, trigger: BargeTrigger, said: &str, whole: bool) {
        if !whole {
            self.user_active(matches!(trigger, BargeTrigger::UserTranscript));
            self.floor.heard();
        }
        let replying = !self.delegation.in_flight() && self.controller.speaking();
        // Nobody talks over a reply that has not sounded yet, and a reply
        // that has is not talked over by its own question.
        let over = match trigger {
            BargeTrigger::UserTranscript => self.question.transcribed(whole, replying && self.ear.sounded()),
            BargeTrigger::SpeechStarted => self.question.speech_started(),
        };
        if over && replying {
            self.talked_over(trigger);
        }
        self.barge.transcribed(said, whole);
        if whole {
            self.user_quiet();
        }
    }

    /// The user talks over a playing reply: renew a pending barge-in, or
    /// arm one if this turn has not cut yet.
    fn talked_over(&mut self, trigger: BargeTrigger) {
        let now = Instant::now();
        if self.barge.pending() {
            self.barge.arm(now, trigger);
            return;
        }
        match self.floor.claim() {
            Err((turn, count)) => self.marks.push(Mark::BargePassed { turn, trigger, count }),
            Ok(_) if !run_marks::local_cut() => {
                // The model judges the turn alone; it is the next reply's question.
                self.question.cut();
                self.spend_floor(|turn| Mark::BargeLeft { turn, trigger });
            }
            Ok(_) => {
                if self.barge.arm(now, trigger) {
                    self.meter.held(now);
                }
            }
        }
    }

    /// The turn had its cut, marked as `mark`; one that already ended
    /// re-arms at once.
    fn spend_floor(&mut self, mark: impl FnOnce(u32) -> Mark) {
        let (turn, rearmed) = self.floor.cut();
        self.marks.push(mark(turn));
        if let Some(turn) = rearmed {
            self.marks.push(Mark::BargeRearmed { turn, reason: "ended" });
        }
    }

    /// The user's turn ended: the latch was `reason`. A turn that cut re-arms.
    pub(in crate::bridge_voice) fn turn_ended(&mut self, reason: &'static str) {
        if let Some(turn) = self.floor.ended(self.barge.pending()) {
            self.marks.push(Mark::BargeRearmed { turn, reason });
        }
    }

    /// When the pending barge-in fires unless the model is heard first.
    #[must_use]
    pub fn barge_due(&self) -> Option<Instant> {
        self.barge.due(Instant::now(), self.reply.finished())
    }

    /// The model has yielded to the user: the pending barge-in cuts the reply.
    /// Retire all old playback state together; never flush an abandoned tail.
    /// Stream order identifies the replacement at its next assistant delta.
    pub fn interrupt(&mut self, port: &mut impl VoicePort) -> Result<(), Error> {
        let Some(trigger) = self.barge.fire(Instant::now(), self.reply.finished()) else {
            return Ok(());
        };
        if self.delegation.in_flight() || !self.controller.speaking() {
            return Ok(());
        }
        let (silent_ms, yield_ms) = self.meter.cut(trigger);
        let text = self.barge.said();
        self.stats.cut = Some(Box::new(Cut {
            text,
            silent_ms,
            yield_ms,
        }));
        self.spend_floor(|turn| Mark::BargeCut { turn, trigger });
        self.stale = true;
        self.question.cut();
        port.retire_audio();
        self.fresh_reply()?;
        self.delegation = Delegation::default();
        self.mouth = super::Mouth::default();
        reply_lips::cut(Instant::now());
        self.controller.cut();
        Ok(())
    }
}
