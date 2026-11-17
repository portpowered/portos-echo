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

    pub fn capture(&mut self, frame: Option<MicFrame>, port: &impl VoicePort) -> Result<(), Error> {
        let Some(frame) = frame else {
            self.microphone_finished = true;
            if let Some(microphone) = &mut self.microphone {
                for frame in microphone.finish()? {
                    port.send(frame)?;
                    self.stats.microphone_frames += 1;
                }
                self.stats.microphone_tail = microphone.tail_samples();
            }
            return Ok(());
        };
        self.stats.microphone_chunks += 1;
        if self.muted {
            return Ok(());
        }
        if self.microphone.is_none() {
            self.microphone = Some(CaptureAudio::new(frame.format)?);
        }
        let microphone = self.microphone.as_mut().ok_or(Error::Audio)?;
        for frame in microphone.process(&frame.format, &frame.payload)? {
            port.send(frame)?;
            self.stats.microphone_frames += 1;
        }
        Ok(())
    }

    pub fn audio(&mut self, frame: &PcmFrame) -> Result<(), Error> {
        self.reply.push(frame)?;
        self.stats.reply_received += 1;
        Ok(())
    }
}
