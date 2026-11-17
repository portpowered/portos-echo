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

    pub fn audio(&mut self, frame: &PcmFrame) -> Result<(), Error> {
        self.reply.push(frame)?;
        self.stats.reply_received += 1;
        Ok(())
    }
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
