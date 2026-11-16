//! Fixed scratch, complete 20 ms packets and explicit partial-tail accounting.
use echo_audio::{OpusDecoder, OpusEncoder, PcmResampler};
use echo_link::PcmFormat;
use echo_live::AudioFrame;

use crate::Error;

pub struct Microphone {
    format: PcmFormat,
    resampler: PcmResampler,
    scratch: Vec<i16>,
    pending: Vec<i16>,
    encoder: OpusEncoder,
    pub input_samples: u64,
    pub packets: u64,
}

impl Microphone {
    /// # Errors
    /// Rejects unsupported PCM formats or unavailable codec resources.
    pub fn new(format: PcmFormat) -> Result<Self, Error> {
        let resampler = PcmResampler::new(&audio_format(format), 48_000)?;
        let scratch = vec![0; resampler.output_capacity()];
        Ok(Self {
            format,
            resampler,
            scratch,
            pending: Vec::with_capacity(960),
            encoder: OpusEncoder::new()?,
            input_samples: 0,
            packets: 0,
        })
    }

    /// The wire parser already bounds payloads; cap one conversion too.
    /// # Errors
    /// Rejects format changes, malformed samples and codec errors.
    pub fn process(&mut self, format: &PcmFormat, bytes: &[u8]) -> Result<Vec<AudioFrame>, Error> {
        if format != &self.format || bytes.len() > 8192 || !bytes.len().is_multiple_of(2) {
            return Err(Error::Audio);
        }
        let input: Vec<i16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        let mut offset = 0;
        let mut packets = Vec::new();
        while offset < input.len() {
            let progress = self.resampler.process(&input[offset..], &mut self.scratch)?;
            offset += progress.consumed;
            self.input_samples += u64::try_from(progress.consumed).map_err(|_| Error::Audio)?;
            self.encode(progress.written, &mut packets)?;
        }
        Ok(packets)
    }

    /// # Errors
    /// Reports resampler or codec failures; never pads an incomplete Opus frame.
    pub fn finish(&mut self) -> Result<Vec<AudioFrame>, Error> {
        let mut packets = Vec::new();
        loop {
            let count = self.resampler.finish(&mut self.scratch)?;
            if count == 0 {
                return Ok(packets);
            }
            self.encode(count, &mut packets)?;
        }
    }

    #[must_use]
    pub fn tail_samples(&self) -> usize {
        self.pending.len()
    }

    fn encode(&mut self, written: usize, packets: &mut Vec<AudioFrame>) -> Result<(), Error> {
        for index in 0..written {
            self.pending.push(self.scratch[index]);
            if self.pending.len() == 960 {
                let mut packet = [0; 1275];
                let count = self.encoder.encode(&self.pending, &mut packet)?;
                let sequence_bytes = self.packets.to_le_bytes();
                let timestamp_bytes = self.packets.wrapping_mul(960).to_le_bytes();
                packets.push(AudioFrame {
                    sequence: u16::from_le_bytes([sequence_bytes[0], sequence_bytes[1]]),
                    timestamp: u32::from_le_bytes(timestamp_bytes[..4].try_into().map_err(|_| Error::Audio)?),
                    payload: packet[..count].to_vec(),
                });
                self.packets += 1;
                self.pending.clear();
            }
        }
        Ok(())
    }
}

pub struct Reply {
    decoder: OpusDecoder,
    resampler: PcmResampler,
    scratch: Vec<i16>,
}

#[must_use]
pub fn format(rate: u32) -> PcmFormat {
    PcmFormat {
        rate,
        width: 2,
        channels: 1,
    }
}

impl Reply {
    /// # Errors
    /// Reports codec or resampler initialization failures.
    pub fn new() -> Result<Self, Error> {
        let resampler = PcmResampler::new(&audio_format(format(48_000)), 24_000)?;
        Ok(Self {
            decoder: OpusDecoder::new()?,
            scratch: vec![0; resampler.output_capacity()],
            resampler,
        })
    }

    /// # Errors
    /// Rejects malformed, stereo or non-20-ms packets.
    pub fn process(&mut self, frame: &AudioFrame) -> Result<Vec<u8>, Error> {
        let mut samples = [0; 960];
        self.decoder.decode(&frame.payload, &mut samples)?;
        let p = self.resampler.process(&samples, &mut self.scratch)?;
        Ok(self.bytes(p.written))
    }

    /// # Errors
    /// Returns the remaining delay-compensated PCM or a conversion error.
    pub fn finish(&mut self) -> Result<Vec<u8>, Error> {
        let mut bytes = Vec::new();
        loop {
            let count = self.resampler.finish(&mut self.scratch)?;
            if count == 0 {
                return Ok(bytes);
            }
            bytes.extend(self.bytes(count));
        }
    }

    fn bytes(&self, count: usize) -> Vec<u8> {
        self.scratch[..count].iter().flat_map(|s| s.to_le_bytes()).collect()
    }
}

// Compatibility with echo-audio's unchanged descriptor stays at this boundary.
fn audio_format(format: PcmFormat) -> echo_contracts::wyoming::AudioFormat {
    echo_contracts::wyoming::AudioFormat {
        rate: format.rate,
        width: format.width,
        channels: format.channels,
        extra: std::collections::BTreeMap::default(),
    }
}
