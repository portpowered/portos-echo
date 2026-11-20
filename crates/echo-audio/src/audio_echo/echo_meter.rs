//! What the canceller took out, measured at its own input and output.
use super::EchoFrame;
use crate::audio_sample::pcm16;

/// Far-end frames with a mean level at or below this are silence.
const AUDIBLE: u32 = 64;
/// How long after an audible far-end frame its echo may still be at the
/// microphone: the longest delay the canceller searches, and a room's tail.
const TAIL_FRAMES: u32 = 100;
/// Short-term levels follow the last half second of frames.
const SMOOTHING: f64 = 0.98;
/// Microphone audio quieter than this, short-term, is not judged.
const JUDGED: f64 = 100.0 * 100.0;
/// The short-term reduction that counts as a converged canceller, in dB.
const CONVERGED_DB10: i32 = 150;

/// One stretch of cancelling, normally one reply. Levels are RMS sample
/// values; `_db10` fields are tenths of a decibel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EchoReport {
    /// 10 ms microphone frames taken while far-end audio could be heard.
    pub frames: u32,
    /// AEC3's estimate of the delay from a far-end frame to its echo. None
    /// before the canceller has run.
    pub delay_ms: Option<u32>,
    /// Echo return loss enhancement as measured: the energy of those
    /// microphone frames before the canceller over their energy after it.
    /// Speech from the room in them lowers it.
    pub erle_db10: i32,
    /// AEC3's own estimate for its linear filter alone. It is clamped low
    /// by design; the suppressor behind the filter does the rest.
    pub filter_erle_db10: i32,
    /// The short-term reduction reached 15 dB in this stretch.
    pub converged: bool,
    /// Far-end time in this stretch before it first did.
    pub converged_after_ms: Option<u32>,
    /// The loudest of those microphone frames before and after.
    pub echo_max: u32,
    pub residual_max: u32,
}

#[derive(Default)]
pub(super) struct EchoMeter {
    /// Microphone frames left in which the far end may still be heard.
    tail: u32,
    report: EchoReport,
    energy_in: f64,
    energy_out: f64,
    recent_in: f64,
    recent_out: f64,
}

fn mean_square(frame: &EchoFrame) -> u32 {
    let sum: u64 = frame.iter().map(|sample| u64::from(sample.unsigned_abs()).pow(2)).sum();
    u32::try_from(sum / 160).unwrap_or(u32::MAX)
}

/// Ten times the level ratio in dB; 0 when either side is silent.
fn db10(over: f64, under: f64) -> i32 {
    if over <= 0.0 || under <= 0.0 {
        return 0;
    }
    i32::from(pcm16(100.0 * (over / under).log10()))
}

impl EchoMeter {
    pub(super) fn rendered(&mut self, far: &EchoFrame) {
        if mean_square(far) > AUDIBLE * AUDIBLE {
            self.tail = TAIL_FRAMES;
        }
    }

    pub(super) fn captured(&mut self, before: &EchoFrame, after: &EchoFrame) {
        if self.tail == 0 {
            return;
        }
        self.tail -= 1;
        let (level_in, level_out) = (mean_square(before), mean_square(after));
        let report = &mut self.report;
        report.frames += 1;
        report.echo_max = report.echo_max.max(level_in.isqrt());
        report.residual_max = report.residual_max.max(level_out.isqrt());
        // A fully cancelled frame still counts as one unit of residue.
        let (level_in, level_out) = (f64::from(level_in), f64::from(level_out.max(1)));
        self.energy_in += level_in;
        self.energy_out += level_out;
        self.recent_in = self.recent_in * SMOOTHING + level_in * (1.0 - SMOOTHING);
        self.recent_out = self.recent_out * SMOOTHING + level_out * (1.0 - SMOOTHING);
        let reduced = self.recent_in >= JUDGED && db10(self.recent_in, self.recent_out) >= CONVERGED_DB10;
        if reduced && !report.converged {
            report.converged = true;
            report.converged_after_ms = Some(report.frames * 10);
        }
    }

    /// Close this stretch. The tail and the short-term levels run on.
    pub(super) fn settle(&mut self, delay_ms: Option<u32>, filter_erle_db10: i32) -> EchoReport {
        let mut report = std::mem::take(&mut self.report);
        report.delay_ms = delay_ms;
        report.filter_erle_db10 = filter_erle_db10;
        report.erle_db10 = db10(self.energy_in, self.energy_out);
        self.energy_in = 0.0;
        self.energy_out = 0.0;
        report
    }
}
