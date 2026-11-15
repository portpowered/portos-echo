//! Bounded, network-free audio conversion for bridge-owned buffers.
mod audio_codec;
mod audio_error;
mod audio_pcm;
mod audio_sample;

pub use audio_codec::{OpusDecoder, OpusEncoder};
pub use audio_error::AudioError;
pub use audio_pcm::{PcmResampler, SampleProgress};

#[cfg(test)]
mod tests {
    use super::*;
    use echo_contracts::wyoming::AudioFormat;
    use std::collections::BTreeMap;

    #[test]
    fn public_pcm_codec_spine_converts_one_complete_frame() {
        let format = AudioFormat {
            rate: 16000,
            width: 2,
            channels: 1,
            extra: BTreeMap::new(),
        };
        let mut converter = PcmResampler::new(&format, 48000).unwrap();
        let mut scratch = vec![0; converter.output_capacity()];
        let progress = converter.process(&[0; 320], &mut scratch).unwrap();
        assert_eq!(progress.consumed, 320);
        let mut frame = scratch[..progress.written].to_vec();
        let tail = converter.finish(&mut scratch).unwrap();
        frame.extend_from_slice(&scratch[..tail]);
        assert_eq!(frame, [0; 960]);
        assert_eq!(converter.finish(&mut scratch).unwrap(), 0);
        let mut encoder = OpusEncoder::new().unwrap();
        let mut packet = [0; 1275];
        let size = encoder.encode(&frame, &mut packet).unwrap();
        let mut decoder = OpusDecoder::new().unwrap();
        let mut reply = [1; 960];
        assert_eq!(decoder.decode(&packet[..size], &mut reply).unwrap(), 960);
        assert_eq!(reply, [0; 960]);
    }
}
