use super::{Error, Stats, VoicePort, session_controller::Controller};
use crate::bridge_codex::{CaptureAudio, ReplyAudio};
use crate::bridge_runtime::bridge_pacing::{Barge, BargeTrigger, Discard, QUIET, ReplyMeter, ReplyQuality};
use echo_link::MicFrame;
use echo_media::PcmFrame;
use std::collections::VecDeque;
use tokio::time::{Duration, Instant};

/// Thirty seconds of 20 ms frames: above the setup capture's own bound.
const UPLINK_BACKLOG: usize = 1500;

pub(super) struct Source {
    pub microphone: Option<CaptureAudio>,
    pub muted: bool,
    pub reply: ReplyAudio,
    pub controller: Controller,
    pub failure: &'static str,
    lead_ms: u16,
    pub delegation: Delegation,
    pub mouth: Mouth,
    pub meter: ReplyMeter,
    pub stats: Stats,
    pub session: super::session_tools::Grants,
    /// Speech the bounded media sender has not taken yet, oldest first.
    uplink: VecDeque<PcmFrame>,
    /// The device was told a dispatch is running in silence.
    thinking: bool,
    /// User speech heard during the reply, until the model yields to it.
    barge: Barge,
}

impl Source {
    pub fn resume_reply(&mut self) -> Result<(), Error> {
        self.controller.resume()?;
        self.fresh_reply()?;
        self.delegation = self.delegation.carried();
        self.mouth = Mouth::default();
        self.barge = Barge::default();
        Ok(())
    }

    /// The device's thinking indication, when it must change. It goes on
    /// when a dispatch runs with nothing to say and off as soon as audible
    /// reply audio is written, so it stays up from a completed turn until its
    /// answer is on its way, and always clears before that reply's audio ends.
    /// The device drops it itself when the run ends.
    fn thinking_change(&mut self) -> Option<bool> {
        let wanted = !self.mouth.voiced() && (self.thinking || self.delegation.in_flight());
        (wanted != self.thinking).then(|| {
            self.thinking = wanted;
            wanted
        })
    }

    /// Replace the reply queue and its pacer, keeping what they counted.
    fn fresh_reply(&mut self) -> Result<(), Error> {
        self.stats.reply_dropped += self.reply.dropped_frames();
        self.meter.paced(self.reply.take_playout());
        self.reply = ReplyAudio::new(self.lead_ms)?;
        Ok(())
    }

    /// The reply has a frame to pace out, or has stalled mid-speech and is
    /// owed a fade-out. A filler that is done has ended, not stalled.
    #[must_use]
    pub fn playable(&self) -> bool {
        self.reply.has_frames() || (self.reply.stalled() && !self.delegation.held())
    }

    /// Close the reply's quality window. `lost` is the call's cumulative RTP loss.
    pub fn settle(&mut self, lost: u64) -> ReplyQuality {
        self.meter.paced(self.reply.take_playout());
        self.meter.settle(lost)
    }

    /// A dispatch that finished while listening gives its answer a fresh
    /// listen window, since the hold on idle ends with it.
    pub fn dispatch_completed(&mut self) {
        let waited = self.delegation.in_flight();
        self.delegation.completed();
        if waited
            && !self.delegation.in_flight()
            && let Some(deadline) = self
                .session
                .listen()
                .and_then(|listen| Instant::now().checked_add(listen))
        {
            self.controller.extend_idle(deadline);
        }
    }

    pub fn new(lead_ms: u16, muted: bool) -> Result<Self, Error> {
        Ok(Self {
            microphone: None,
            muted,
            reply: ReplyAudio::new(lead_ms)?,
            controller: Controller::default(),
            failure: "playback",
            lead_ms,
            delegation: Delegation::default(),
            mouth: Mouth::default(),
            thinking: false,
            meter: ReplyMeter::default(),
            stats: Stats::default(),
            session: super::session_tools::Grants::default(),
            uplink: VecDeque::new(),
            barge: Barge::default(),
        })
    }

    /// A sign of user speech during a reply arms a barge-in; it cuts nothing
    /// yet, because the model may talk on (`Barge`).
    /// A dispatch in flight is thinking, not a reply to talk over: user speech
    /// neither cancels the delegated turn nor cuts the speaker until it
    /// completes. The Live session still hears that speech.
    pub fn heard_user(&mut self, trigger: BargeTrigger) {
        if !self.delegation.in_flight() && self.controller.speaking() {
            self.barge.arm(Instant::now(), trigger);
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
        self.meter.cut(trigger);
        port.retire_audio();
        self.fresh_reply()?;
        self.delegation = Delegation::default();
        self.mouth = Mouth::default();
        self.controller.cut();
        Ok(())
    }

    /// Return whether this chunk was suppressed by the defensive mute gate.
    /// Adapter-suppressed chunks arrive separately as ordered device events.
    pub fn capture(&mut self, frame: Option<MicFrame>, port: &impl VoicePort) -> Result<bool, Error> {
        let Some(frame) = frame else {
            self.controller.microphone_ended();
            if let Some(microphone) = &mut self.microphone {
                let frames = microphone.finish()?;
                self.stats.microphone_tail = microphone.tail_samples();
                for frame in frames {
                    self.forward(&frame, port)?;
                }
            }
            return Ok(false);
        };
        self.stats.microphone_chunks += 1;
        if self.muted {
            return Ok(true);
        }
        if self.microphone.is_none() {
            self.microphone = Some(CaptureAudio::new(frame.format)?);
        }
        let microphone = self.microphone.as_mut().ok_or(Error::Audio)?;
        for frame in microphone.process(&frame.format, &frame.payload)? {
            self.forward(&frame, port)?;
        }
        Ok(false)
    }

    /// Speech captured while the call was set up is replayed as one burst, far
    /// faster than the bounded media sender encodes it. Keep what the sender
    /// refuses, in order, instead of letting it evict the start of the request.
    fn forward(&mut self, frame: &PcmFrame, port: &impl VoicePort) -> Result<(), Error> {
        self.flush_uplink(port)?;
        if !self.uplink.is_empty() || !port.try_send(*frame)? {
            if self.uplink.len() == UPLINK_BACKLOG {
                self.uplink.pop_front();
                self.stats.uplink_dropped += 1;
            }
            self.uplink.push_back(*frame);
        }
        self.stats.microphone_frames += 1;
        Ok(())
    }

    /// Hand the oldest waiting speech to the sender until it is full again.
    pub fn flush_uplink(&mut self, port: &impl VoicePort) -> Result<(), Error> {
        while let Some(frame) = self.uplink.front() {
            if !port.try_send(*frame)? {
                break;
            }
            self.uplink.pop_front();
        }
        Ok(())
    }

    #[must_use]
    pub fn uplink_pending(&self) -> bool {
        !self.uplink.is_empty()
    }

    /// Mute withholds everything not yet handed to the media sender.
    pub fn mute_uplink(&mut self) {
        self.microphone = None;
        self.uplink.clear();
    }

    /// Continuous RTP received after a completed reply belongs to the retired
    /// converter. Discard both quiet and audible media at that boundary.
    pub fn audio(&mut self, frame: &PcmFrame) -> Result<(), Error> {
        self.stats.reply_received += 1;
        let reason = if self.controller.cutting() || self.controller.retired() {
            Discard::Cut
        } else if !self.controller.accepts_audio() {
            Discard::Idle
        } else if self.reply.finished() || self.delegation.abandoned {
            Discard::AfterReply
        } else {
            // PCM has no utterance identity. Only ordered assistant control
            // opens Speaking; the initial user transcript can lag audible RTP.
            let now = Instant::now();
            self.barge.heard(now, audible(frame));
            let wanted = self.meter.arrived(now);
            self.meter.targeted(self.reply.retarget(wanted));
            return self.reply.push(frame);
        };
        let audible = audible(frame);
        self.meter.discarded(reason, audible);
        Ok(())
    }
}

/// The call frame holds more than WebRTC silence.
fn audible(frame: &PcmFrame) -> bool {
    frame.iter().any(|sample| sample.unsigned_abs() > QUIET)
}

/// Preserve playback ownership until the device acknowledges an interrupted
/// reply. A cancellation cannot safely resume the connection after a timeout.
pub(super) async fn settle_cut(
    playback: impl std::future::Future<Output = Result<(), Error>>,
    result: &std::cell::Cell<Option<Result<echo_link::PlayOutcome, echo_link::Error>>>,
    deadline: std::time::Duration,
) -> Result<(), Error> {
    if result.get().is_none() {
        // Cut maps to Wyoming in the ordinary successful-reply path. Here the
        // recorded outcome distinguishes an acknowledged cut from transport loss.
        let _ = tokio::time::timeout(deadline, playback)
            .await
            .map_err(|_| Error::Deadline)?;
    }
    match result.get() {
        Some(Ok(_)) => Ok(()),
        Some(Err(error)) => Err(error.into()),
        None => Err(Error::Wyoming),
    }
}

/// Failure-only, closed-vocabulary diagnostics: never include device frames,
/// provider text, addresses or credentials. Preserve the original failure.
pub(super) async fn cut_generation(
    client: &impl echo_link::EchoLink,
    playback: impl std::future::Future<Output = Result<(), Error>>,
    result: &std::cell::Cell<Option<Result<echo_link::PlayOutcome, echo_link::Error>>>,
    deadline: std::time::Duration,
    source: &Source,
    sink_open: bool,
) -> Result<(), Error> {
    let cut = tokio::time::timeout(std::time::Duration::from_secs(1), client.cut_with_stats()).await;
    let settled = async {
        let stats = cut.map_err(|_| Error::Deadline)??;
        // The cut discarded reply frames the paused clock counted as accepted.
        if let Some(clock) = crate::bridge_mcp::bridge_clock::receipt::Context::current() {
            clock.discarded("reply", stats.dropped_frames)?;
        }
        settle_cut(playback, result, deadline).await
    }
    .await;
    if let Err(error) = &settled {
        playback_failure("cut_ack", error, source, sink_open, result.get());
    }
    settled
}

pub(super) fn playback_failure(
    stage: &'static str,
    error: &Error,
    source: &Source,
    sink_open: bool,
    playback: Option<Result<echo_link::PlayOutcome, echo_link::Error>>,
) {
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr(),
        "echo-bridge: wyoming stage={stage} error={error:?} state={:?} sink_open={sink_open} finished={} drained={} played={} cut_receipts={} playback={playback:?}",
        source.controller,
        source.reply.finished(),
        source.reply.drained(),
        source.controller.has_played(),
        source.controller.pending_cut_receipts(),
    );
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

#[derive(Default)]
pub struct Delegation {
    turns: usize,
    armed: bool,
    utterance: Utterance,
    held: bool,
    abandoned: bool,
}

impl Delegation {
    pub fn started(&mut self) {
        self.turns += 1;
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

    /// The next reply starts clean but still owes each running turn its
    /// completion; an utterance after the reset is an answer, not a filler.
    fn carried(&self) -> Self {
        Self {
            turns: self.turns,
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
        !self.held
    }

    /// A filler done is holding the reply open for a delegated answer.
    #[must_use]
    pub fn held(&self) -> bool {
        self.held && !self.abandoned
    }

    /// The response deadline passed first: the reply ends after the filler and
    /// the answer, if it still comes, is dropped instead of failing the run.
    pub fn abandon(&mut self) {
        self.abandoned = true;
    }
}

const FRAME: Duration = Duration::from_millis(20);
/// Measured on the real Echo: hassmic reports `played` 0.07 to 0.24 s (typically
/// 0.22 s) after the modelled playout clock below drains.
const OUTPUT_DELAY: Duration = Duration::from_millis(200);

/// The screen's mouth follows audio the Echo is actually playing, not model
/// events. Frames written to the Echo play back to back on its clock, which
/// restarts when the device ran dry. The mouth opens when the first audible
/// frame reaches the speaker and closes when the last one has played.
#[derive(Default)]
pub struct Mouth {
    clock: Option<Instant>,
    transitions: VecDeque<(Instant, bool)>,
    open: bool,
}

/// Best effort: the indication is display only, so a device that cannot take
/// it never fails or delays the reply.
pub(super) async fn think<L: echo_link::EchoLink>(client: &L, source: &mut Source) {
    if let Some(on) = source.thinking_change() {
        let _ = tokio::time::timeout(Duration::from_secs(1), client.thinking(on)).await;
    }
}

impl Mouth {
    /// Record one 24 kHz s16le frame written to the Echo at `now`.
    pub fn wrote(&mut self, frame: &[u8], now: Instant) {
        if frame.is_empty() {
            return;
        }
        let start = self.clock.filter(|end| *end > now).unwrap_or(now);
        self.clock = Some(start + FRAME);
        let audible = frame
            .as_chunks::<2>()
            .0
            .iter()
            .any(|sample| i16::from_le_bytes(*sample).unsigned_abs() > QUIET);
        if audible {
            let open = start + OUTPUT_DELAY;
            let close = open + FRAME;
            if let Some((due, false)) = self.transitions.back_mut()
                && *due == open
            {
                *due = close;
            } else {
                self.transitions.push_back((open, true));
                self.transitions.push_back((close, false));
            }
        }
    }

    /// When the next open or close is due.
    #[must_use]
    pub fn due(&self) -> Option<Instant> {
        self.transitions.front().map(|(due, _)| *due)
    }

    /// The change due by `now`, if any: true opens the mouth, false closes it.
    pub fn poll(&mut self, now: Instant) -> Option<bool> {
        self.due().filter(|due| *due <= now)?;
        let (_, open) = self.transitions.pop_front()?;
        self.open = open;
        Some(open)
    }

    /// Audible reply audio is written to the Echo and not yet played out.
    #[must_use]
    pub fn voiced(&self) -> bool {
        self.open || self.due().is_some()
    }
}

#[cfg(test)]
#[path = "earcon/voice_source_tests.rs"]
mod tests;
