use crate::AudioError;

const FRAME: usize = 960;
const PACKET: usize = 1275;

/// Single-owner 48 kHz mono encoder. Recreate after a native processing error.
pub struct OpusEncoder {
    inner: opus::Encoder,
    failed: bool,
}

impl OpusEncoder {
    /// Fixed 64 kbps audio application, without DTX or FEC.
    /// # Errors
    /// Returns the native codec's initialization/configuration error.
    pub fn new() -> Result<Self, AudioError> {
        let mut inner = opus::Encoder::new(48_000, opus::Channels::Mono, opus::Application::Audio)?;
        inner.set_bitrate(opus::Bitrate::Bits(64_000))?;
        inner.set_dtx(false)?;
        inner.set_inband_fec(false)?;
        Ok(Self { inner, failed: false })
    }

    /// Codec-reported delay in samples, for timestamping and quality alignment.
    /// # Errors
    /// Returns a codec control error.
    pub fn lookahead(&mut self) -> Result<i32, AudioError> {
        Ok(self.inner.get_lookahead()?)
    }

    /// Encode exactly 20 ms; packet storage must hold the worst-case packet.
    /// Never pads implicitly. Invalid arguments leave encoder state unchanged.
    /// # Errors
    /// Returns Frame, Output, Poisoned, or a native processing failure.
    pub fn encode(&mut self, samples: &[i16], packet: &mut [u8]) -> Result<usize, AudioError> {
        if self.failed {
            return Err(AudioError::Poisoned);
        }
        if samples.len() != FRAME {
            return Err(AudioError::Frame);
        }
        if packet.len() < PACKET {
            return Err(AudioError::Output);
        }
        let result = self.inner.encode(samples, &mut packet[..PACKET]);
        self.failed = result.is_err();
        Ok(result?)
    }
}

/// Single-owner decoder; malformed packets never request packet-loss concealment.
/// Recreate after a native processing error.
pub struct OpusDecoder {
    inner: opus::Decoder,
    failed: bool,
}

impl OpusDecoder {
    /// Create a 48 kHz mono decoder.
    /// # Errors
    /// Returns a codec initialization error.
    pub fn new() -> Result<Self, AudioError> {
        Ok(Self {
            inner: opus::Decoder::new(48_000, opus::Channels::Mono)?,
            failed: false,
        })
    }

    /// Decode one complete mono 20 ms packet into 960 samples.
    /// # Errors
    /// Returns Packet, Output, Poisoned, or a native processing failure.
    pub fn decode(&mut self, packet: &[u8], samples: &mut [i16]) -> Result<usize, AudioError> {
        if self.failed {
            return Err(AudioError::Poisoned);
        }
        validate_packet(packet)?;
        if samples.len() < FRAME {
            return Err(AudioError::Output);
        }
        let result = self.inner.decode(packet, &mut samples[..FRAME], false);
        self.failed = result.is_err();
        Ok(result?)
    }
}

fn validate_packet(packet: &[u8]) -> Result<(), AudioError> {
    if packet.is_empty() || packet.len() > PACKET {
        return Err(AudioError::Packet);
    }
    let parsed = opus::packet::parse(packet).map_err(|_| AudioError::Packet)?;
    let duration = opus::packet::get_nb_samples(packet, 48_000).map_err(|_| AudioError::Packet)?;
    let channels = opus::packet::get_nb_channels(packet).map_err(|_| AudioError::Packet)?;
    if duration != FRAME || channels != opus::Channels::Mono || parsed.frames.is_empty() {
        return Err(AudioError::Packet);
    }
    Ok(())
}

#[cfg(test)]
mod audio_codec_tests;
