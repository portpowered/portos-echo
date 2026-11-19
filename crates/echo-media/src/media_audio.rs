// Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0
// Adapted from Codex voice-host processing.rs; Modified by Port OS.
use crate::{Error, PcmFrame};
use rubato::{Resampler, audioadapter_buffers::direct::InterleavedSlice};
use std::collections::VecDeque;

pub(crate) struct Converter {
    engine: rubato::Async<f32>,
    scratch: Vec<f32>,
    output: VecDeque<f32>,
}
impl Converter {
    fn new(input: usize, ratio: f64) -> Result<Self, Error> {
        let engine = rubato::Async::new_sinc(
            ratio,
            1.0,
            &rubato::SincInterpolationParameters::default(),
            input,
            1,
            rubato::FixedAsync::Input,
        )
        .map_err(|_| Error::Audio)?;
        let scratch = vec![0.0; engine.output_frames_max()];
        Ok(Self {
            engine,
            scratch,
            output: VecDeque::new(),
        })
    }
    fn push(&mut self, samples: &[f32]) -> Result<(), Error> {
        let input = InterleavedSlice::new(samples, 1, samples.len()).map_err(|_| Error::Audio)?;
        let capacity = self.scratch.len();
        let mut output = InterleavedSlice::new_mut(&mut self.scratch, 1, capacity).map_err(|_| Error::Audio)?;
        let (_, produced) = self
            .engine
            .process_into_buffer(&input, &mut output, None)
            .map_err(|_| Error::Audio)?;
        self.output.extend(&self.scratch[..produced]);
        Ok(())
    }
    fn take<const N: usize>(&mut self) -> Option<[f32; N]> {
        if self.output.len() < N {
            return None;
        }
        let mut block = [0.0; N];
        for (out, sample) in block.iter_mut().zip(self.output.drain(..N)) {
            *out = sample;
        }
        Some(block)
    }
}

pub(crate) struct Encoder {
    converter: Converter,
    codec: opus::Encoder,
}
impl Encoder {
    pub(crate) fn new() -> Result<Self, Error> {
        Ok(Self {
            converter: Converter::new(320, 3.0)?,
            codec: opus::Encoder::new(48_000, opus::Channels::Mono, opus::Application::Voip)
                .map_err(|_| Error::Audio)?,
        })
    }
    pub(crate) fn encode(&mut self, pcm: &PcmFrame) -> Result<Option<Vec<u8>>, Error> {
        let input = pcm.map(|sample| f32::from(sample) / 32768.0);
        self.converter.push(&input)?;
        let Some(samples) = self.converter.take::<960>() else {
            return Ok(None);
        };
        let mut packet = vec![0; 1275];
        let len = self
            .codec
            .encode_float(&samples, &mut packet)
            .map_err(|_| Error::Audio)?;
        packet.truncate(len);
        Ok(Some(packet))
    }
}

pub(crate) struct Decoder {
    converter: Converter,
    codec: opus::Decoder,
    sequence: Option<u16>,
    stream: Option<u32>,
    /// Sequence numbers skipped by accepted packets since creation.
    pub(crate) lost: u64,
    /// A reset retired queued ingress: the next gap is the boundary, not loss.
    resync: bool,
}
impl Decoder {
    pub(crate) fn new() -> Result<Self, Error> {
        Ok(Self {
            converter: Converter::new(960, 1.0 / 3.0)?,
            codec: opus::Decoder::new(48_000, opus::Channels::Mono).map_err(|_| Error::Audio)?,
            sequence: None,
            stream: None,
            lost: 0,
            resync: false,
        })
    }
    pub(crate) fn reset(&mut self) -> Result<(), Error> {
        // Sequence/SSRC ownership survives replacement; a replay is still stale.
        self.converter = Converter::new(960, 1.0 / 3.0)?;
        self.codec = opus::Decoder::new(48_000, opus::Channels::Mono).map_err(|_| Error::Audio)?;
        self.resync = true;
        Ok(())
    }
    pub(crate) fn decode(&mut self, packet: &rtc::rtp::Packet) -> Result<Option<PcmFrame>, Error> {
        let sequence = packet.header.sequence_number;
        if self.stream.is_some_and(|ssrc| ssrc != packet.header.ssrc) {
            return Err(Error::Transport);
        }
        if self
            .sequence
            .is_some_and(|last| sequence.wrapping_sub(last) >= 32768 || sequence == last)
        {
            return Ok(None);
        }
        if packet.header.payload_type != 111 || packet.payload.is_empty() || packet.payload.len() > 1275 {
            return Err(Error::Transport);
        }
        let mut samples = [0.0; 960];
        let count = self
            .codec
            .decode_float(&packet.payload, &mut samples, false)
            .map_err(|_| Error::Audio)?;
        if count != 960 {
            return Err(Error::Audio);
        }
        if let Some(last) = self.sequence
            && !std::mem::take(&mut self.resync)
        {
            self.lost += u64::from(sequence.wrapping_sub(last) - 1);
        }
        self.sequence = Some(sequence);
        self.stream = Some(packet.header.ssrc);
        self.converter.push(&samples)?;
        // Conversion clamps normalized floating point to the PCM16 domain.
        Ok(self
            .converter
            .take::<320>()
            .map(|frame| frame.map(|s| pcm16(f64::from(s) * 32768.0))))
    }
}

// Same checked IEEE-754 conversion used by echo-audio; no unchecked float cast.
pub(crate) fn pcm16(value: f64) -> i16 {
    let rounded = value.round().clamp(-32768.0, 32767.0);
    if rounded.abs() < 1.0 || rounded.is_nan() {
        return 0;
    }
    let bits = rounded.abs().to_bits();
    let exponent = (bits >> 52) & 0x7ff;
    let significand = (bits & ((1_u64 << 52) - 1)) | (1_u64 << 52);
    let magnitude = i32::try_from(significand >> (1075 - exponent)).unwrap_or(32768);
    let signed = if rounded.is_sign_negative() {
        -magnitude
    } else {
        magnitude
    };
    i16::try_from(signed).unwrap_or(if signed < 0 { i16::MIN } else { i16::MAX })
}

#[cfg(test)]
mod media_audio_tests {
    use super::*;

    #[test]
    fn failure_resampler_rejects_invalid_ratio_and_short_input() {
        assert!(Converter::new(320, 0.0).is_err());
        let mut converter = Converter::new(320, 3.0).unwrap();
        assert_eq!(converter.push(&[]), Err(Error::Audio));
        assert_eq!(converter.take::<960>(), None);
        converter.push(&[0.0; 320]).unwrap();
    }

    #[test]
    fn pcm_conversion_clamps_and_rounds_without_wrapping() {
        for sample in [i16::MIN, -1, 0, 1, i16::MAX] {
            assert_eq!(pcm16(f64::from(sample)), sample);
        }
        assert_eq!(pcm16(f64::NAN), 0);
        assert_eq!(pcm16(f64::INFINITY), i16::MAX);
        assert_eq!(pcm16(f64::NEG_INFINITY), i16::MIN);
        assert_eq!(pcm16(0.5), 1);
        assert_eq!(pcm16(-0.5), -1);
    }
}
