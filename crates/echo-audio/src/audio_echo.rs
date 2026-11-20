//! WebRTC's echo canceller (AEC3, the `sonora` port) for one mono microphone.
use crate::{AudioError, audio_sample::pcm16};
use sonora::config::{EchoCanceller as Aec3, EchoCanceller3Config, HighPassFilter};
use sonora::{AudioProcessing, Config, StreamConfig};

mod echo_meter;
#[cfg(any(test, feature = "fixture"))]
pub mod echo_room;
use echo_meter::EchoMeter;
pub use echo_meter::EchoReport;

/// The rate the canceller runs at: the microphone's own.
pub const ECHO_RATE: u32 = 16_000;
/// Samples in one 10 ms frame, the only size AEC3 takes.
pub const ECHO_FRAME: usize = 160;
/// One 10 ms frame of signed 16-bit mono at `ECHO_RATE`.
pub type EchoFrame = [i16; ECHO_FRAME];

/// Each of AEC3's matched filters adds 96 ms of delay search to the first
/// one's 128 ms: eight find an echo up to about 800 ms behind its far end.
const DELAY_FILTERS: usize = 8;
/// The longest echo path delay a hint may name.
pub const ECHO_DELAY_MAX_MS: u16 = 800;

/// Far-end frames in, microphone frames through. The caller keeps the two
/// streams in step: one far-end frame, played or silent, before each
/// microphone frame. AEC3 then finds the delay between a far-end frame and
/// its echo by itself and follows it when it moves.
///
/// Only the echo canceller and the high-pass filter it needs are on. Noise
/// suppression and gain control stay off: nothing measured asks for them.
pub struct EchoCanceller {
    apm: AudioProcessing,
    scratch: EchoFrame,
    meter: EchoMeter,
}

impl EchoCanceller {
    /// `delay_hint_ms` is where AEC3 looks for the echo first, until its own
    /// estimator has heard enough far-end audio to know.
    #[must_use]
    pub fn new(delay_hint_ms: u16) -> Self {
        let stream = StreamConfig::new(ECHO_RATE, 1);
        let hint = delay_hint_ms.min(ECHO_DELAY_MAX_MS);
        let mut aec3 = EchoCanceller3Config::default();
        aec3.delay.num_filters = DELAY_FILTERS;
        // In blocks of 4 ms. The stream delay below says the same where the
        // library accepts it (to 500 ms).
        aec3.delay.default_delay = usize::from(hint / 4);
        let config = Config {
            echo_canceller: Some(Aec3::default()),
            high_pass_filter: Some(HighPassFilter::default()),
            ..Config::default()
        };
        let mut apm = AudioProcessing::builder()
            .config(config)
            .capture_config(stream)
            .render_config(stream)
            .echo_canceller3_config(aec3)
            .build();
        // A clamped hint is still a hint.
        let _ = apm.set_stream_delay_ms(i32::from(hint));
        Self {
            apm,
            scratch: [0; ECHO_FRAME],
            meter: EchoMeter::default(),
        }
    }

    /// One frame the speaker plays, or silence when it plays nothing.
    /// # Errors
    /// Reports a frame the processor refused.
    pub fn render(&mut self, far: &EchoFrame) -> Result<(), AudioError> {
        self.meter.rendered(far);
        self.apm
            .process_render_i16(far, &mut self.scratch)
            .map_err(|_| AudioError::Echo)
    }

    /// One microphone frame, replaced by itself without the echo.
    /// # Errors
    /// Reports a frame the processor refused; the frame is then unchanged.
    pub fn capture(&mut self, near: &mut EchoFrame) -> Result<(), AudioError> {
        self.apm
            .process_capture_i16(near, &mut self.scratch)
            .map_err(|_| AudioError::Echo)?;
        self.meter.captured(near, &self.scratch);
        *near = self.scratch;
        Ok(())
    }

    /// What the canceller did since this was last asked, and where AEC3
    /// stands now. The filter and the delay it found are kept.
    pub fn report(&mut self) -> EchoReport {
        let stats = self.apm.statistics();
        let delay_ms = stats.delay_ms.and_then(|delay| u32::try_from(delay).ok());
        let filter = stats.echo_return_loss_enhancement.filter(|db| db.is_finite());
        self.meter
            .settle(delay_ms, filter.map_or(0, |db| i32::from(pcm16(db * 10.0))))
    }
}

#[cfg(test)]
#[path = "audio_echo/audio_echo_tests.rs"]
mod tests;
