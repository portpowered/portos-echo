/// Failure categories contain no audio payloads.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("unsupported PCM format")]
    Format,
    #[error("input must contain complete sample frames")]
    Input,
    #[error("output buffer is too small")]
    Output,
    #[error("expected one 960-sample Opus frame")]
    Frame,
    #[error("expected a valid 20 ms Opus packet of at most 1275 bytes")]
    Packet,
    #[error("processor failed; reset or recreate it before reuse")]
    Poisoned,
    #[error("stream has finished; reset before adding input")]
    Finished,
    #[error("native codec failure: {0}")]
    Codec(#[from] opus::Error),
    #[error("resampler configuration failure: {0}")]
    Configuration(#[from] rubato::ResamplerConstructionError),
    #[error("resampler processing failure: {0}")]
    Resample(#[from] rubato::ResampleError),
}

#[cfg(test)]
mod tests {
    use super::AudioError;

    #[test]
    fn public_errors_describe_recovery_without_audio_payloads() {
        let cases = [
            (AudioError::Format, "unsupported PCM format"),
            (AudioError::Input, "input must contain complete sample frames"),
            (AudioError::Output, "output buffer is too small"),
            (AudioError::Frame, "expected one 960-sample Opus frame"),
            (
                AudioError::Packet,
                "expected a valid 20 ms Opus packet of at most 1275 bytes",
            ),
            (
                AudioError::Poisoned,
                "processor failed; reset or recreate it before reuse",
            ),
            (AudioError::Finished, "stream has finished; reset before adding input"),
        ];
        for (error, message) in cases {
            assert_eq!(error.to_string(), message);
        }
    }
}
