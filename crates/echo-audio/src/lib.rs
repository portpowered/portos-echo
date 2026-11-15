//! Bounded, network-free audio conversion for bridge-owned buffers.
mod audio_codec;
mod audio_error;
mod audio_pcm;
mod audio_sample;

pub use audio_codec::{OpusDecoder, OpusEncoder};
pub use audio_error::AudioError;
pub use audio_pcm::{PcmResampler, SampleProgress};
