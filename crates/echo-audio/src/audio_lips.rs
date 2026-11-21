//! Mouth features of one short window of speech, for lip sync: how far the
//! mouth is open, from loudness, and which vowel it shapes, from the two
//! lowest formants. Plain arithmetic on one window at a time: no model, no
//! allocation after the first window, about forty Goertzel probes per window.
use std::f32::consts::TAU;

/// The five mouth shapes a character can draw beyond closed, half and open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vowel {
    A,
    I,
    U,
    E,
    O,
}

/// One window's mouth: openness in percent, and the vowel when it is clear.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LipSample {
    pub open: u8,
    pub vowel: Option<Vowel>,
}

/// How loudness becomes openness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LipOptions {
    /// Window RMS at or below this is a closed mouth: the noise floor.
    pub floor: u16,
    /// Window RMS at or above this is a fully open mouth: the gain.
    pub full: u16,
    /// Percent of the way to a wider mouth covered in one window.
    pub attack: u8,
    /// Percent points the mouth may close in one window.
    pub release: u8,
}

impl Default for LipOptions {
    fn default() -> Self {
        Self {
            floor: 250,
            full: 5000,
            attack: 70,
            release: 40,
        }
    }
}

/// Formant centres in Hz, between typical adult voices.
const CENTRES: [(Vowel, f32, f32); 5] = [
    (Vowel::A, 800.0, 1300.0),
    (Vowel::I, 300.0, 2500.0),
    (Vowel::U, 350.0, 1000.0),
    (Vowel::E, 500.0, 2000.0),
    (Vowel::O, 500.0, 900.0),
];
/// The first formant is looked for up to here, in 50 Hz steps from 200 Hz.
const FIRST_TOP: u16 = 1000;
/// The second up to here, in 100 Hz steps, clear of the first one's skirt.
const SECOND_TOP: u16 = 3100;
const SECOND_FLOOR: f32 = 850.0;
const SKIRT: f32 = 300.0;
/// A vowel is clear when its formants sit within this many octaves, summed,
/// of a centre, and nearer to it than to the next centre by the margin.
const NEAR: f32 = 0.7;
const MARGIN: f32 = 0.05;
/// A second formant weaker than this share of the first is not a formant.
const SECOND_SHARE: f32 = 0.0025;
/// More sign changes than one sample in this many is hiss, not a vowel.
const HISS: usize = 5;
/// Windows longer than this are analysed by their first samples only.
const LONGEST: usize = 2048;

struct Probe {
    hertz: f32,
    coefficient: f32,
}

/// Follows one stream of signed 16-bit little-endian mono windows.
pub struct LipAnalyser {
    options: LipOptions,
    probes: Vec<Probe>,
    taper: Vec<f32>,
    shaped: Vec<f32>,
    powers: Vec<(f32, f32)>,
    open: u8,
    seen: Option<Vowel>,
    held: Option<Vowel>,
}

impl LipAnalyser {
    #[must_use]
    pub fn new(rate: u16, options: LipOptions) -> Self {
        let lower = (200..=FIRST_TOP).step_by(50);
        let upper = (FIRST_TOP + 100..=SECOND_TOP).step_by(100);
        let nyquist = rate / 2;
        let probes = lower
            .chain(upper)
            .filter(|hertz| *hertz < nyquist)
            .map(|hertz| Probe {
                hertz: f32::from(hertz),
                coefficient: 2.0 * (TAU * f32::from(hertz) / f32::from(rate.max(1))).cos(),
            })
            .collect();
        Self {
            options,
            probes,
            taper: Vec::new(),
            shaped: Vec::new(),
            powers: Vec::new(),
            open: 0,
            seen: None,
            held: None,
        }
    }

    /// The mouth is closed and nothing is ringing down.
    #[must_use]
    pub fn at_rest(&self) -> bool {
        self.open == 0
    }

    /// Analyse the next window. Silence costs one pass for its level.
    pub fn window(&mut self, pcm: &[u8]) -> LipSample {
        let (samples, _) = pcm.as_chunks::<2>();
        let samples = &samples[..samples.len().min(LONGEST)];
        let target = self.target(level(samples));
        self.open = if target >= self.open {
            let gap = u16::from(target - self.open) * u16::from(self.options.attack.min(100));
            self.open + u8::try_from(gap.div_ceil(100)).unwrap_or(u8::MAX)
        } else {
            target.max(self.open.saturating_sub(self.options.release.max(1)))
        };
        let seen = if target == 0 { None } else { self.vowel(samples) };
        // One window alone does not change the shape: a guess must repeat.
        if seen == self.seen || target == 0 {
            self.held = seen;
        }
        self.seen = seen;
        LipSample {
            open: self.open,
            vowel: self.held.filter(|_| self.open > 0),
        }
    }

    /// Openness the level asks for: linear above the floor, then a square
    /// root, so quiet speech still moves the mouth.
    fn target(&self, level: u32) -> u8 {
        let floor = u32::from(self.options.floor);
        let span = u32::from(self.options.full).saturating_sub(floor).max(1);
        let linear = (level.saturating_sub(floor) * 100 / span).min(100);
        u8::try_from((linear * 100).isqrt()).unwrap_or(100)
    }

    fn vowel(&mut self, samples: &[[u8; 2]]) -> Option<Vowel> {
        let crossings = samples
            .windows(2)
            .filter(|pair| (i16::from_le_bytes(pair[0]) < 0) != (i16::from_le_bytes(pair[1]) < 0))
            .count();
        if samples.len() < 64 || crossings * HISS > samples.len() {
            return None;
        }
        self.shape(samples);
        self.powers.clear();
        for probe in &self.probes {
            self.powers
                .push((probe.hertz, goertzel(&self.shaped, probe.coefficient)));
        }
        let first = peak(
            self.powers
                .iter()
                .filter(|(hertz, _)| *hertz <= f32::from(FIRST_TOP))
                .copied(),
        )?;
        let above = SECOND_FLOOR.max(first.0 + SKIRT);
        // Voiced sound falls away with frequency: the second formant is
        // found, and weighed against the first, with that slope taken out.
        let lifted = self.powers.iter().map(|(hertz, power)| (*hertz, power * hertz * hertz));
        let second = peak(lifted.filter(|(hertz, _)| *hertz >= above))?;
        if second.1 < first.1 * first.0 * first.0 * SECOND_SHARE {
            return None;
        }
        nearest(self.centre(first.0, 150.0), self.centre(second.0, 200.0))
    }

    /// Where the energy around a peak sits: a formant between two harmonics
    /// of the voice lies between them, not on the stronger one.
    fn centre(&self, peak: f32, reach: f32) -> f32 {
        let near = self.powers.iter().filter(|(hertz, _)| (hertz - peak).abs() <= reach);
        let (weighted, total) = near.fold((0.0, 0.0), |(weighted, total), (hertz, power)| {
            (weighted + hertz * power, total + power)
        });
        if total > 0.0 { weighted / total } else { peak }
    }

    /// The taper keeps one probe's energy out of its neighbours.
    fn shape(&mut self, samples: &[[u8; 2]]) {
        if self.taper.len() != samples.len() {
            let last = f32::from(u16::try_from(samples.len().saturating_sub(1)).unwrap_or(u16::MAX)).max(1.0);
            self.taper = (0..u16::try_from(samples.len()).unwrap_or(u16::MAX))
                .map(|index| 0.5 - 0.5 * (TAU * f32::from(index) / last).cos())
                .collect();
        }
        self.shaped.clear();
        for (sample, taper) in samples.iter().zip(&self.taper) {
            self.shaped.push(f32::from(i16::from_le_bytes(*sample)) * taper);
        }
    }
}

/// Root mean square of the window.
fn level(samples: &[[u8; 2]]) -> u32 {
    let sum = samples.iter().fold(0_u64, |sum, sample| {
        let value = u64::from(i16::from_le_bytes(*sample).unsigned_abs());
        sum + value * value
    });
    let count = u64::try_from(samples.len()).unwrap_or(u64::MAX).max(1);
    u32::try_from((sum / count).isqrt()).unwrap_or(u32::MAX)
}

/// Signal power at one frequency.
fn goertzel(signal: &[f32], coefficient: f32) -> f32 {
    let (mut one, mut two) = (0.0_f32, 0.0_f32);
    for value in signal {
        let next = value + coefficient * one - two;
        two = one;
        one = next;
    }
    one * one + two * two - coefficient * one * two
}

fn peak(powers: impl Iterator<Item = (f32, f32)>) -> Option<(f32, f32)> {
    powers
        .filter(|(_, power)| *power > 0.0)
        .max_by(|left, right| left.1.total_cmp(&right.1))
}

/// The centre nearest in octaves, when it is near and clearly the nearest.
fn nearest(first: f32, second: f32) -> Option<Vowel> {
    let mut ranked = CENTRES.map(|(vowel, one, two)| ((first / one).log2().abs() + (second / two).log2().abs(), vowel));
    ranked.sort_by(|left, right| left.0.total_cmp(&right.0));
    let [(best, vowel), (next, _), ..] = ranked;
    (best <= NEAR && next - best >= MARGIN).then_some(vowel)
}

#[cfg(test)]
#[path = "audio_lips/audio_lips_tests.rs"]
mod tests;
