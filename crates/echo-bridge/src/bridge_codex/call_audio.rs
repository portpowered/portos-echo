//! PCM boundaries for the run owner; media owns Opus and the 48 kHz track.
use crate::{Error, bridge_world::PacerPort};
use echo_audio::PcmResampler;
use echo_core_api::{Command, PacerCommand, PacerEvent, ReplyFrame};
use echo_link::PcmFormat;
use echo_media::PcmFrame;
use echo_sub_pacer::{Heard, LipStats, Playout, State};
use std::time::Duration;
use tokio::time::Instant;

struct Converter {
    resampler: PcmResampler,
    scratch: Vec<i16>,
}

impl Converter {
    fn new(format: PcmFormat, target: u32) -> Result<Self, Error> {
        let format = echo_contracts::wyoming::AudioFormat {
            rate: format.rate,
            width: format.width,
            channels: format.channels,
            extra: std::collections::BTreeMap::default(),
        };
        let resampler = PcmResampler::new(&format, target)?;
        Ok(Self {
            scratch: vec![0; resampler.output_capacity()],
            resampler,
        })
    }

    fn process(&mut self, samples: &[i16], mut emit: impl FnMut(&[i16])) -> Result<(), Error> {
        let mut offset = 0;
        while offset < samples.len() {
            let progress = self.resampler.process(&samples[offset..], &mut self.scratch)?;
            offset += progress.consumed;
            emit(&self.scratch[..progress.written]);
        }
        Ok(())
    }

    fn finish(&mut self, mut emit: impl FnMut(&[i16])) -> Result<(), Error> {
        loop {
            let count = self.resampler.finish(&mut self.scratch)?;
            if count == 0 {
                return Ok(());
            }
            emit(&self.scratch[..count]);
        }
    }
}

/// Accept bounded Wyoming chunks and emit only complete mono/16 kHz frames.
/// Drop this owner on mute/cut to discard both filter history and partial input.
pub struct CaptureAudio {
    format: PcmFormat,
    converter: Converter,
    pending: Vec<i16>,
    finished: bool,
}

impl CaptureAudio {
    /// # Errors
    /// Unsupported PCM formats fail before any samples are accepted.
    pub fn new(format: PcmFormat) -> Result<Self, Error> {
        Ok(Self {
            format,
            converter: Converter::new(format, 16_000)?,
            pending: Vec::with_capacity(320),
            finished: false,
        })
    }

    /// # Errors
    /// Reject format changes, oversized/noninterleaved chunks and post-finish input.
    pub fn process(&mut self, format: &PcmFormat, bytes: &[u8]) -> Result<Vec<PcmFrame>, Error> {
        let stride = 2 * usize::from(self.format.channels);
        if self.finished || format != &self.format || bytes.len() > 8192 || !bytes.len().is_multiple_of(stride) {
            return Err(Error::Audio);
        }
        let samples: Vec<_> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        let mut frames = Vec::new();
        let pending = &mut self.pending;
        self.converter
            .process(&samples, |samples| frame_capture(pending, samples, &mut frames))?;
        Ok(frames)
    }

    /// Drain filter delay, keeping an incomplete 20 ms tail unpadded and unsent.
    /// # Errors
    /// Returns a fixed conversion error. Repeated finish emits no additional frames.
    pub fn finish(&mut self) -> Result<Vec<PcmFrame>, Error> {
        let mut frames = Vec::new();
        self.finished = true;
        let pending = &mut self.pending;
        self.converter
            .finish(|samples| frame_capture(pending, samples, &mut frames))?;
        Ok(frames)
    }

    /// Number of unsent mono/16 kHz samples, after finish for final tail accounting.
    #[must_use]
    pub fn tail_samples(&self) -> usize {
        self.pending.len()
    }
}

fn frame_capture(pending: &mut Vec<i16>, samples: &[i16], frames: &mut Vec<PcmFrame>) {
    for &sample in samples {
        pending.push(sample);
        if pending.len() == 320 {
            let mut frame = [0; 320];
            frame.copy_from_slice(pending);
            frames.push(frame);
            pending.clear();
        }
    }
}

/// Resampling and the synchronous service port to the World-owned reply.
pub struct ReplyAudio {
    converter: Converter,
    port: PacerPort,
    scratch: Option<Box<ReplyFrame>>,
    partial: usize,
    microphone: Option<Box<PcmFrame>>,
}

pub(crate) struct ReplySettlement {
    pub(crate) playout: Playout,
    pub(crate) heard: Heard,
    pub(crate) lip_stats: LipStats,
}

fn reply_converter() -> Result<Converter, Error> {
    Converter::new(crate::bridge_audio::format(16_000), 24_000)
}

impl ReplyAudio {
    /// # Errors
    /// Reports PCM converter initialization failure.
    pub fn new(lead_ms: u16) -> Result<Self, Error> {
        let adaptive = crate::bridge_mcp::bridge_clock::receipt::Context::current().is_none();
        Self::with_pacing(lead_ms, adaptive)
    }

    fn with_pacing(lead_ms: u16, adaptive: bool) -> Result<Self, Error> {
        Ok(Self {
            converter: reply_converter()?,
            port: PacerPort::open(lead_ms, adaptive),
            scratch: Some(Box::new([0; 960])),
            partial: 0,
            microphone: Some(Box::new([0; 320])),
        })
    }

    /// # Errors
    /// Rejects post-finish input and converter failures without decoding Opus again.
    pub fn push(&mut self, frame: &PcmFrame) -> Result<(), Error> {
        if self.finished() {
            return Err(Error::Audio);
        }
        let (port, scratch, partial) = (&self.port, &mut self.scratch, &mut self.partial);
        self.converter
            .process(frame, |samples| frame_reply(port, scratch, partial, samples))
    }

    /// Flush accepted PCM exactly once. Every input frame yields 480 output samples.
    /// # Errors
    /// Reports converter failure or an incomplete output frame; never pads a tail.
    pub fn finish(&mut self) -> Result<(), Error> {
        let (port, scratch, partial) = (&self.port, &mut self.scratch, &mut self.partial);
        port.send(PacerEvent::InputFinished);
        self.converter
            .finish(|samples| frame_reply(port, scratch, partial, samples))?;
        if *partial != 0 {
            return Err(Error::Audio);
        }
        Ok(())
    }

    pub(crate) fn reset(&mut self, cut: bool) -> Result<Playout, Error> {
        let playout = self.port.read(State::playout);
        self.port
            .send(if cut { PacerEvent::Cut } else { PacerEvent::ReplyOpened });
        self.partial = 0;
        self.converter = reply_converter()?;
        Ok(playout)
    }

    pub(crate) fn discard_output(&mut self) {
        self.partial = 0;
        self.port.send(PacerEvent::RemoteEnd);
    }

    pub(crate) fn arrived(&self, audible: bool) -> Duration {
        self.port.send(PacerEvent::PeerFrameArrived { audible });
        self.port.read(State::target)
    }

    pub(crate) fn discarded(&self) {
        self.port.send(PacerEvent::PeerFrameDiscarded);
    }

    pub(crate) fn microphone(&mut self, frame: &PcmFrame) {
        let Some(mut scratch) = self.microphone.take() else {
            return;
        };
        *scratch = *frame;
        let mut event = PacerEvent::MicrophoneFrame(scratch);
        self.port.dispatch(&mut event);
        if let PacerEvent::MicrophoneFrame(scratch) = event {
            self.microphone = Some(scratch);
        }
    }

    /// Cancellation while waiting consumes no PCM. The single staged frame
    /// remains in World until the transport takes it or the reply is cut.
    pub(crate) async fn stage(&mut self, allow_fade: bool) -> Option<ReplyFrame> {
        let mut out = self.port.send(PacerEvent::Offer { allow_fade });
        loop {
            if matches!(out.notification(), Some(Command::Pacer(PacerCommand::WriteFrame))) {
                return out.take_frame();
            }
            let deadline = self.port.playout_due()?;
            tokio::time::sleep_until(deadline).await;
            out = self.port.send(PacerEvent::Tick);
        }
    }

    pub(crate) fn handed(&self) {
        self.port.handed();
    }

    pub(crate) fn played(&self) -> bool {
        matches!(
            self.port.send(PacerEvent::Played).notification(),
            Some(Command::Pacer(PacerCommand::PlayoutFinished))
        )
    }

    pub(crate) fn settle(&self) -> ReplySettlement {
        let (playout, heard) = self.port.read(|state| (state.playout(), state.heard()));
        self.port.send(PacerEvent::ReplySettled);
        ReplySettlement {
            playout,
            heard,
            lip_stats: self.port.take_lip_stats(),
        }
    }

    pub(crate) fn mouth_settle(&self) {
        self.port.send(PacerEvent::MouthSettled);
    }

    pub(crate) fn mouth_due(&self) -> Option<Instant> {
        self.port.instant(self.port.read(State::mouth_due))
    }

    pub(crate) fn mouth_change(&self) -> Option<bool> {
        self.port.mouth_change()
    }

    pub(crate) fn sounded(&self) -> bool {
        self.port.read(State::sounded)
    }

    pub(crate) fn voiced(&self) -> bool {
        self.port.read(State::view).playing
    }

    #[must_use]
    pub fn has_frames(&self) -> bool {
        self.port.read(State::view).depth != 0
    }

    #[must_use]
    pub fn stalled(&self) -> bool {
        self.port.read(State::stalled)
    }

    #[must_use]
    pub fn spoken(&self) -> bool {
        self.port.read(State::spoken)
    }

    #[must_use]
    pub fn finished(&self) -> bool {
        self.port.read(State::finished)
    }

    /// Transferred all retained frames. The device still owes a Played receipt.
    #[must_use]
    pub fn drained(&self) -> bool {
        self.partial == 0 && self.port.read(State::drained)
    }

    #[must_use]
    pub fn dropped_frames(&self) -> u64 {
        self.port.read(State::dropped)
    }

    /// Receive and acknowledge one queued frame at the standalone PCM boundary.
    pub async fn next_frame(&mut self) -> Option<Vec<u8>> {
        let frame = self.stage(false).await?;
        self.handed();
        Some(frame.to_vec())
    }

    #[cfg(test)]
    pub(crate) fn mouth_fixture() -> Self {
        Self::with_pacing(u16::MAX, false).unwrap()
    }

    #[cfg(test)]
    pub(crate) fn push_pcm(&mut self, frame: &ReplyFrame) {
        **self.scratch.as_mut().unwrap() = *frame;
        push_frame(&self.port, &mut self.scratch);
    }

    #[cfg(test)]
    pub(crate) fn test_handoff(&mut self, frame: &[u8]) {
        if frame.is_empty() {
            return;
        }
        self.push_pcm(frame.try_into().unwrap());
        assert!(
            self.port
                .send(PacerEvent::Offer { allow_fade: false })
                .take_frame()
                .is_some()
        );
        self.handed();
    }
}

fn frame_reply(port: &PacerPort, scratch: &mut Option<Box<ReplyFrame>>, partial: &mut usize, samples: &[i16]) {
    for sample in samples {
        let Some(buffer) = scratch.as_mut() else { return };
        buffer[*partial..*partial + 2].copy_from_slice(&sample.to_le_bytes());
        *partial += 2;
        if *partial == buffer.len() {
            push_frame(port, scratch);
            *partial = 0;
        }
    }
}

fn push_frame(port: &PacerPort, scratch: &mut Option<Box<ReplyFrame>>) {
    // Temporarily move the buffer into the synchronous event and recover it.
    let Some(frame) = scratch.take() else { return };
    let mut event = PacerEvent::PcmPushed(frame);
    port.dispatch(&mut event);
    if let PacerEvent::PcmPushed(frame) = event {
        *scratch = Some(frame);
    }
}

#[cfg(test)]
#[path = "call_audio_tests.rs"]
mod tests;
