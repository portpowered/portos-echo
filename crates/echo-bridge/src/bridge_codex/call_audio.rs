//! PCM boundaries for the run owner; media owns Opus and the 48 kHz track.
use crate::{
    Error,
    bridge_runtime::bridge_pacing::{ClockPacer, Playout},
};
use echo_audio::{FrameQueue, PcmResampler};
use echo_link::PcmFormat;
use echo_media::PcmFrame;
use std::num::NonZeroUsize;

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

/// Bounded Wyoming PCM24 reply queue, paced on the audio clock up to the lead.
/// `finish` is legal only after the caller has drained accepted media; transcript
/// completion alone cannot establish that. Empty/drained never proves `played`.
pub struct ReplyAudio {
    converter: Converter,
    queue: FrameQueue<Vec<u8>>,
    partial: Vec<u8>,
    pacer: ClockPacer<Vec<u8>>,
    finished: bool,
    /// The reply has taken audible audio: it is speaking, not just opened.
    spoken: bool,
}

impl ReplyAudio {
    /// # Errors
    /// Reports PCM converter initialization failure.
    pub fn new(lead_ms: u16) -> Result<Self, Error> {
        Ok(Self {
            converter: Converter::new(crate::bridge_audio::format(16_000), 24_000)?,
            queue: FrameQueue::new(NonZeroUsize::new(128).ok_or(Error::Audio)?),
            partial: Vec::with_capacity(960),
            pacer: ClockPacer::new(lead_ms),
            finished: false,
            spoken: false,
        })
    }

    /// # Errors
    /// Rejects post-finish input and converter failures without decoding Opus again.
    pub fn push(&mut self, frame: &PcmFrame) -> Result<(), Error> {
        if self.finished {
            return Err(Error::Audio);
        }
        let (queue, partial) = (&mut self.queue, &mut self.partial);
        self.converter
            .process(frame, |samples| frame_reply(queue, partial, samples))
    }

    /// Flush accepted PCM exactly once. Every input frame yields 480 output samples.
    /// # Errors
    /// Reports converter failure or an incomplete output frame; never pads a tail.
    pub fn finish(&mut self) -> Result<(), Error> {
        self.finished = true;
        let (queue, partial) = (&mut self.queue, &mut self.partial);
        self.converter.finish(|samples| frame_reply(queue, partial, samples))?;
        if !self.partial.is_empty() {
            return Err(Error::Audio);
        }
        Ok(())
    }

    /// Cancellation while waiting consumes no PCM. Frames are never sent more
    /// than the lead ahead of the audio clock; late wakeups catch up. A dry
    /// device is primed first (`ClockPacer`).
    pub async fn next_frame(&mut self) -> Option<Vec<u8>> {
        self.pacer.next_frame(&mut self.queue, self.finished).await
    }

    /// Whether a paced receive can eventually yield a retained frame.
    #[must_use]
    pub fn has_frames(&self) -> bool {
        !self.queue.is_empty()
    }

    /// The unfinished reply has nothing queued and its last frame sent was
    /// mid-speech: if nothing more comes, the device is owed a fade-out.
    #[must_use]
    pub fn stalled(&self) -> bool {
        self.queue.is_empty() && self.pacer.conceals(self.finished)
    }

    /// The next queued frame as `next_frame` paces it or, when stalled, one
    /// frame that fades the reply to silence just before the device runs dry.
    /// Cancellation while waiting changes nothing.
    pub async fn next_or_fade(&mut self) -> Option<Vec<u8>> {
        if self.queue.is_empty() {
            self.pacer.fade_out(self.finished).await
        } else {
            self.next_frame().await
        }
    }

    /// Hold this much reply audio ahead of playout from the next dry start;
    /// returns the cushion in force.
    pub(crate) fn retarget(&mut self, wanted: std::time::Duration) -> std::time::Duration {
        self.pacer.retarget(wanted)
    }

    /// What the pacer did since this was last asked.
    pub(crate) fn take_playout(&mut self) -> Playout {
        self.pacer.take_playout()
    }

    /// Audible audio was accepted into this reply.
    pub fn speak(&mut self) {
        self.spoken = true;
    }

    /// The reply settled: what it said is no longer being spoken.
    pub fn hush(&mut self) {
        self.spoken = false;
    }

    /// Whether this reply has accepted audible audio since it opened.
    #[must_use]
    pub fn spoken(&self) -> bool {
        self.spoken
    }

    /// The reply was flushed; later peer media is not part of it.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.finished
    }

    /// Accepted media was flushed and all retained frames were handed to playback.
    /// The run owner must still await Wyoming's played receipt.
    #[must_use]
    pub fn drained(&self) -> bool {
        self.finished && self.partial.is_empty() && self.queue.is_empty()
    }

    #[must_use]
    pub fn dropped_frames(&self) -> u64 {
        self.queue.drop_count()
    }
}

fn frame_reply(queue: &mut FrameQueue<Vec<u8>>, partial: &mut Vec<u8>, samples: &[i16]) {
    for sample in samples {
        partial.extend_from_slice(&sample.to_le_bytes());
        if partial.len() == 960 {
            queue.push(std::mem::replace(partial, Vec::with_capacity(960)));
        }
    }
}

#[cfg(test)]
#[path = "call_audio_tests.rs"]
mod tests;
