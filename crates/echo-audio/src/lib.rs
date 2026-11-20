//! Bounded, network-free audio conversion for bridge-owned buffers.
mod audio_codec;
mod audio_echo;
mod audio_error;
mod audio_pacer;
mod audio_pcm;
mod audio_queue;
mod audio_sample;

pub use audio_codec::{OpusDecoder, OpusEncoder};
#[cfg(any(test, feature = "fixture"))]
pub use audio_echo::echo_room;
pub use audio_echo::{ECHO_DELAY_MAX_MS, ECHO_FRAME, ECHO_RATE, EchoCanceller, EchoFrame, EchoReport};
pub use audio_error::AudioError;
pub use audio_pacer::Pacer;
pub use audio_pcm::{PcmResampler, SampleProgress};
pub use audio_queue::FrameQueue;

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
