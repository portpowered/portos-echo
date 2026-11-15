use crate::{AudioError, audio_sample::pcm16};
use echo_contracts::wyoming::AudioFormat;
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};

/// Counts are i16 elements: consumed includes all interleaved input channels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SampleProgress {
    pub consumed: usize,
    pub written: usize,
}

/// Bounded single-owner PCM16 converter. Output is mono at the chosen rate.
/// Calls accept complete interleaved sample frames, at most 20 ms per call;
/// retry the unconsumed suffix. No audio is retained beyond fixed scratch.
pub struct PcmResampler {
    inner: Option<SincFixedIn<f64>>,
    input: Vec<f64>,
    output: Vec<f64>,
    buffered: usize,
    channels: usize,
    input_rate: u32,
    output_rate: u32,
    accepted: u64,
    emitted: u64,
    skip: usize,
    finishing: bool,
    failed: bool,
}

impl PcmResampler {
    /// Validate the existing Wyoming format (width in bytes) without changing it.
    /// # Errors
    /// Returns Format for unsupported rates, widths or channel counts, or a
    /// resampler configuration error.
    pub fn new(format: &AudioFormat, output_rate: u32) -> Result<Self, AudioError> {
        let supported = |rate| matches!(rate, 16_000 | 22_050 | 24_000 | 48_000);
        if !supported(format.rate) || !supported(output_rate) || format.width != 2 || !matches!(format.channels, 1 | 2)
        {
            return Err(AudioError::Format);
        }
        let block = usize::try_from(format.rate / 50).map_err(|_| AudioError::Format)?;
        let inner = if format.rate == output_rate {
            None
        } else {
            Some(SincFixedIn::new(
                f64::from(output_rate) / f64::from(format.rate),
                1.0,
                SincInterpolationParameters {
                    sinc_len: 128,
                    f_cutoff: 0.95,
                    interpolation: SincInterpolationType::Cubic,
                    oversampling_factor: 128,
                    window: WindowFunction::BlackmanHarris2,
                },
                block,
                1,
            )?)
        };
        let skip = inner.as_ref().map_or(0, Resampler::output_delay);
        let maximum = inner.as_ref().map_or(block, Resampler::output_frames_max);
        Ok(Self {
            inner,
            input: vec![0.0; block],
            output: vec![0.0; maximum],
            buffered: 0,
            channels: usize::from(format.channels),
            input_rate: format.rate,
            output_rate,
            accepted: 0,
            emitted: 0,
            skip,
            finishing: false,
            failed: false,
        })
    }

    /// Required output slice capacity for process and finish, fixed per instance.
    #[must_use]
    pub fn output_capacity(&self) -> usize {
        self.output.len()
    }

    /// Consume at most one input block and emit available delay-compensated PCM.
    /// Stereo uses the widened arithmetic mean, rounding toward zero.
    /// Validation and insufficient output space consume nothing.
    /// # Errors
    /// Returns Input, Output, Finished, Poisoned, or a native resampler error.
    /// A native error poisons the processor until reset.
    pub fn process(&mut self, input: &[i16], output: &mut [i16]) -> Result<SampleProgress, AudioError> {
        self.validate_output(output)?;
        if self.finishing {
            return Err(AudioError::Finished);
        }
        if !input.len().is_multiple_of(self.channels) {
            return Err(AudioError::Input);
        }
        let frames = (input.len() / self.channels).min(self.input.len() - self.buffered);
        for (frame, target) in input
            .chunks_exact(self.channels)
            .take(frames)
            .zip(&mut self.input[self.buffered..])
        {
            let sum: i32 = frame.iter().map(|&sample| i32::from(sample)).sum();
            *target = f64::from(sum / i32::try_from(self.channels).map_err(|_| AudioError::Format)?);
        }
        self.buffered += frames;
        self.accepted += u64::try_from(frames).map_err(|_| AudioError::Input)?;
        let written = if self.inner.is_none() {
            self.copy_identity(output)
        } else if self.buffered == self.input.len() {
            self.run_block(output)?
        } else {
            0
        };
        Ok(SampleProgress {
            consumed: frames * self.channels,
            written,
        })
    }

    /// Zero-pad bounded blocks to drain filter delay and trim to
    /// floor(input mono frames * output rate / input rate). Call until zero is
    /// returned. Repeated/empty finish emits nothing; process then requires reset.
    /// # Errors
    /// Returns Output, Poisoned, or a resampler error; reset after native failure.
    pub fn finish(&mut self, output: &mut [i16]) -> Result<usize, AudioError> {
        self.validate_output(output)?;
        self.finishing = true;
        if self.emitted == self.target_length() {
            return Ok(0);
        }
        loop {
            self.input[self.buffered..].fill(0.0);
            let written = self.run_block(output)?;
            if written != 0 {
                return Ok(written);
            }
        }
    }

    /// Discard old stream data and filter phase without reallocating scratch.
    pub fn reset(&mut self) {
        if let Some(inner) = &mut self.inner {
            inner.reset();
        }
        self.skip = self.inner.as_ref().map_or(0, Resampler::output_delay);
        self.input.fill(0.0);
        self.output.fill(0.0);
        self.buffered = 0;
        self.accepted = 0;
        self.emitted = 0;
        self.finishing = false;
        self.failed = false;
    }

    fn validate_output(&self, output: &[i16]) -> Result<(), AudioError> {
        if self.failed {
            return Err(AudioError::Poisoned);
        }
        if output.len() < self.output_capacity() {
            return Err(AudioError::Output);
        }
        Ok(())
    }

    fn target_length(&self) -> u64 {
        // Quotient/remainder keeps the multiplication bounded for long streams.
        let rate = u64::from(self.input_rate);
        (self.accepted / rate) * u64::from(self.output_rate)
            + (self.accepted % rate) * u64::from(self.output_rate) / rate
    }

    fn copy_identity(&mut self, output: &mut [i16]) -> usize {
        let count = self.buffered;
        for (target, &sample) in output.iter_mut().zip(&self.input[..count]) {
            *target = pcm16(sample);
        }
        self.emitted = self.accepted;
        self.buffered = 0;
        count
    }

    fn run_block(&mut self, output: &mut [i16]) -> Result<usize, AudioError> {
        let Some(inner) = &mut self.inner else {
            return Ok(0);
        };
        let result = inner.process_into_buffer(&[&self.input], &mut [&mut self.output], None);
        self.failed = result.is_err();
        let (_, produced) = result?;
        self.buffered = 0;
        let skipped = self.skip.min(produced);
        self.skip -= skipped;
        let remaining = usize::try_from(self.target_length() - self.emitted).unwrap_or(usize::MAX);
        let count = (produced - skipped).min(remaining);
        for (target, &sample) in output.iter_mut().zip(&self.output[skipped..skipped + count]) {
            *target = pcm16(sample);
        }
        self.emitted += u64::try_from(count).map_err(|_| AudioError::Output)?;
        Ok(count)
    }
}

#[cfg(test)]
mod audio_pcm_tests;
