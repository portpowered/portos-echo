use super::{Error, Stats, VoicePort};
use crate::bridge_codex::{CaptureAudio, ReplyAudio};
use echo_link::MicFrame;
use echo_media::PcmFrame;

pub(super) struct Source {
    pub microphone: Option<CaptureAudio>,
    pub microphone_finished: bool,
    pub muted: bool,
    pub reply: ReplyAudio,
    pub played: bool,
    pub delegation: Delegation,
    pub stats: Stats,
}

impl Source {
    pub fn new(lead_ms: u16, muted: bool) -> Result<Self, Error> {
        Ok(Self {
            microphone: None,
            microphone_finished: false,
            muted,
            reply: ReplyAudio::new(lead_ms)?,
            played: false,
            delegation: Delegation::default(),
            stats: Stats::default(),
        })
    }

    /// Return whether this chunk was suppressed by the defensive mute gate.
    /// Adapter-suppressed chunks arrive separately as ordered device events.
    pub fn capture(&mut self, frame: Option<MicFrame>, port: &impl VoicePort) -> Result<bool, Error> {
        let Some(frame) = frame else {
            self.microphone_finished = true;
            if let Some(microphone) = &mut self.microphone {
                for frame in microphone.finish()? {
                    port.send(frame)?;
                    self.stats.microphone_frames += 1;
                }
                self.stats.microphone_tail = microphone.tail_samples();
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
            port.send(frame)?;
            self.stats.microphone_frames += 1;
        }
        Ok(false)
    }

    /// WebRTC media is continuous: quiet frames keep arriving after the
    /// assistant transcript is done and the reply was flushed. They are counted
    /// as received but not queued; audible late media still fails closed
    /// rather than truncating the reply silently, except an abandoned
    /// delegated answer.
    pub fn audio(&mut self, frame: &PcmFrame) -> Result<(), Error> {
        let dropped = self.delegation.abandoned || quiet(frame);
        if !(self.reply.finished() && dropped) {
            self.reply.push(frame)?;
        }
        self.stats.reply_received += 1;
        Ok(())
    }
}

/// Peak at most 64 (about -54 dBFS): decoded silence, not speech.
fn quiet(frame: &PcmFrame) -> bool {
    frame.iter().all(|sample| sample.unsigned_abs() <= 64)
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
