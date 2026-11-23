use super::{Error, VoicePort, session_controller::Controller};
use crate::bridge_codex::{CaptureAudio, ReplyAudio};
use crate::bridge_runtime::bridge_pacing::{Cut, Discard, QUIET, ReplyMeter, ReplyQuality};
use echo_core_api::{CallCommand, CallCommands, SessionOutcome, SessionTurnEvent, UplinkEvent, UplinkReceipt};
use echo_link::MicFrame;
use echo_media::PcmFrame;
use tokio::time::Instant;

#[path = "recovery/voice_floor.rs"]
mod voice_floor;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub received_chunks: u64,
    pub dropped_chunks: u64,
    pub microphone_chunks: u64,
    pub microphone_frames: u64,
    pub microphone_tail: usize,
    /// Speech evicted because the media sender stalled past the backlog bound.
    pub uplink_dropped: u64,
    pub reply_received: u64,
    pub reply_sent: u64,
    pub reply_dropped: u64,
    /// The settled reply's pacing and discard facts, boxed to keep notices small.
    pub quality: Box<ReplyQuality>,
    /// The barge-in that cut the settled reply.
    pub cut: Option<Box<Cut>>,
}

pub(super) struct Source {
    pub microphone: Option<CaptureAudio>,
    pub muted: bool,
    pub reply: ReplyAudio,
    pub controller: Controller,
    pub failure: &'static str,
    pub meter: ReplyMeter,
    pub stats: Stats,
    /// The device was told a dispatch is running in silence.
    thinking: bool,
}

impl Source {
    pub fn resume_reply(&mut self, preserve_dispatch: bool) -> Result<(), Error> {
        self.controller.resume(preserve_dispatch)?;
        self.fresh_reply(false)
    }

    /// The device's thinking indication, when it must change. It goes on
    /// when a dispatch runs with nothing to say and off as soon as audible
    /// reply audio is written, so it stays up from a completed turn until its
    /// answer is on its way, and always clears before that reply's audio ends.
    /// The device drops it itself when the run ends.
    pub(super) fn thinking_change(&mut self) -> Option<bool> {
        let wanted = !self.reply.voiced() && (self.thinking || self.controller.dispatch_in_flight());
        (wanted != self.thinking).then(|| {
            self.thinking = wanted;
            wanted
        })
    }

    /// Replace the reply queue and its pacer, keeping what they counted.
    pub(super) fn fresh_reply(&mut self, cut: bool) -> Result<(), Error> {
        self.stats.reply_dropped += self.reply.dropped_frames();
        self.meter.paced(self.reply.reset(cut)?);
        Ok(())
    }

    /// The reply has a frame to pace out, or has stalled mid-speech and is
    /// owed a fade-out. A filler that is done has ended, not stalled. A
    /// pending barge-in holds the reply: the device plays out what it has.
    #[must_use]
    pub fn playable(&self) -> bool {
        !self.controller.barge_pending()
            && (self.reply.has_frames() || (self.reply.stalled() && !self.controller.dispatch_held()))
    }

    /// The assistant began an utterance: call audio is a reply's again.
    pub fn spoke(&mut self) {
        self.controller.reply_started();
    }

    /// Call audio and a done still belong to a reply that was cut.
    #[must_use]
    pub fn stale(&self) -> bool {
        self.controller.stale()
    }

    /// Give the configured listen window from now.
    pub fn renew_idle(&mut self) {
        self.controller.renew_idle();
    }

    /// Close the reply's quality window. `lost` is the call's cumulative RTP loss.
    pub fn settle(&mut self, lost: u64) -> ReplyQuality {
        let pacer = self.reply.settle();
        self.meter.paced(pacer.playout);
        let mut quality = self.meter.settle(lost);
        quality.mic = pacer.heard;
        quality.lips = pacer.lip_stats;
        if quality.cuts == 0 {
            self.stats.cut = None;
        }
        self.controller.turn(|session| SessionTurnEvent::ReplyMeasured {
            session,
            cut: quality.cuts != 0,
        });
        quality
    }

    /// One reply frame was handed to the Echo.
    pub fn wrote(&mut self) {
        let now = Instant::now();
        self.reply.handed();
        if self.reply.voiced() {
            self.controller
                .turn(|session| SessionTurnEvent::ReplyFrameHanded { session, voiced: true });
        }
        self.meter.wrote(now);
    }

    pub(super) fn played(&mut self, stopped: bool) -> Result<bool, Error> {
        let receipt = match self.controller.played(stopped) {
            Ok(false) => return Ok(false),
            Ok(true) if self.reply.played() => return Ok(true),
            Ok(true) => Error::Wyoming,
            Err(error) => error,
        };
        self.failure = "premature_played";
        Err(receipt)
    }

    /// A dispatch that finished while listening gives its answer a fresh
    /// listen window, since the hold on idle ends with it.
    pub fn dispatch_completed(&self) {
        self.controller.dispatch_completed(false, true);
    }

    #[cfg(test)]
    pub fn new(lead_ms: u16, muted: bool) -> Result<Self, Error> {
        Self::with_session(lead_ms, muted, Instant::now(), None)
    }

    #[cfg(test)]
    pub(super) fn with_session(
        lead_ms: u16,
        muted: bool,
        opened: Instant,
        max: Option<std::time::Duration>,
    ) -> Result<Self, Error> {
        Self::with_controller(lead_ms, muted, Controller::open(opened, max)?)
    }

    pub(super) fn with_controller(lead_ms: u16, muted: bool, controller: Controller) -> Result<Self, Error> {
        Ok(Self {
            microphone: None,
            muted,
            reply: ReplyAudio::new(lead_ms)?,
            controller,
            failure: "playback",
            thinking: false,
            meter: ReplyMeter::default(),
            stats: Stats::default(),
        })
    }

    /// An applied `end_session` supersedes everything (owner, 2026-11-21):
    /// nothing delegated is owed any more, and a reply opened for it stops at
    /// once, as a barge-in cut does. A reply already audible is the Live
    /// voice's farewell for the end's hand-off: it plays out, and the run ends
    /// at its playback boundary. Between replies there is nothing to stop.
    /// True when that farewell is done and only held for the delegated answer.
    pub fn end_if_ended(&mut self, port: &mut impl VoicePort) -> Result<bool, Error> {
        if !self.controller.ending() || self.controller.cutting() || self.controller.closing() {
            return Ok(false);
        }
        let between = self.controller.idle_deadline().is_some();
        let spoken = self.reply.spoken();
        let finish = self.controller.turn(|session| SessionTurnEvent::EndDispatch {
            session,
            farewell: !between && spoken,
        });

        if between {
            return Ok(false);
        }
        if spoken {
            return Ok(finish.outcome == Some(Ok(SessionOutcome::FinishReply)));
        }
        port.retire_audio();
        self.fresh_reply(true)?;
        self.controller.cut();
        Ok(false)
    }

    /// Return whether this chunk was suppressed by the defensive mute gate.
    /// Adapter-suppressed chunks arrive separately as ordered device events.
    pub fn capture(&mut self, frame: Option<MicFrame>, port: &impl VoicePort) -> Result<bool, Error> {
        let Some(frame) = frame else {
            self.controller.microphone_ended();
            self.user_quiet();
            if let Some(microphone) = &mut self.microphone {
                let result = microphone
                    .finish()?
                    .iter()
                    .try_for_each(|frame| forward(&self.controller, &mut self.stats, frame, port));
                self.stats.microphone_tail = microphone.tail_samples();
                result?;
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
            // The microphone is measured, never judged: every frame goes up.
            self.reply.microphone(frame);
            forward(&self.controller, &mut self.stats, frame, port)?;
        }
        Ok(false)
    }

    /// Hand the oldest waiting speech to the sender until it is full again.
    pub fn flush_uplink(&mut self, port: &impl VoicePort) -> Result<(), Error> {
        flush_uplink(&self.controller, port)
    }

    #[must_use]
    pub fn uplink_pending(&self) -> bool {
        self.controller.uplink_pending()
    }

    /// Mute withholds everything not yet handed to the media sender.
    pub fn mute_uplink(&mut self) {
        self.microphone = None;
        self.controller.uplink(UplinkEvent::DropBacklog);
    }

    /// Continuous RTP received after a completed reply belongs to the retired
    /// converter. Discard both quiet and audible media at that boundary.
    pub fn audio(&mut self, frame: &PcmFrame) -> Result<(), Error> {
        self.stats.reply_received += 1;
        let reason = if self.stale() {
            Discard::Cut
        } else if !self.controller.accepts_audio() {
            Discard::Idle
        } else if self.reply.finished() {
            Discard::AfterReply
        } else {
            // PCM has no utterance identity. Only ordered assistant control
            // opens Speaking; the initial user transcript can lag audible RTP.
            let now = Instant::now();
            let audible = audible(frame);
            if self.controller.assistant_released_barge(audible) {
                self.meter.released(now);
            }
            self.meter.arrived(now);
            self.meter.targeted(self.reply.arrived(audible));
            return self.reply.push(frame);
        };
        let audible = audible(frame);
        self.meter.discarded(reason, audible);
        self.reply.discarded();
        Ok(())
    }
}

/// Setup speech can arrive faster than the bounded sender encodes it. Give
/// waiting speech the first opportunity, then queue or offer the new frame.
fn forward(controller: &Controller, stats: &mut Stats, frame: &PcmFrame, port: &impl VoicePort) -> Result<(), Error> {
    flush_uplink(controller, port)?;
    let offer = controller.uplink(UplinkEvent::MicFrame(frame));
    stats.uplink_dropped += u64::from(offer.uplink_evicted);
    send_uplink(controller, offer, port)?;
    stats.microphone_frames += 1;
    Ok(())
}

fn flush_uplink(controller: &Controller, port: &impl VoicePort) -> Result<(), Error> {
    while send_uplink(controller, controller.uplink(UplinkEvent::Writable), port)? {}
    Ok(())
}

fn send_uplink(controller: &Controller, offer: CallCommands, port: &impl VoicePort) -> Result<bool, Error> {
    let (Some(CallCommand::SendUplinkFrame { attempt, .. }), Some(frame)) = offer.into_parts() else {
        return Ok(false);
    };
    let result = port.try_send(frame);
    let receipt = match result {
        Ok(true) => UplinkReceipt::Accepted,
        Ok(false) => UplinkReceipt::Full,
        Err(_) => UplinkReceipt::Failed,
    };
    controller.uplink(UplinkEvent::Result { attempt, receipt });
    result
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

#[cfg(test)]
#[path = "earcon/voice_source_tests.rs"]
mod tests;
