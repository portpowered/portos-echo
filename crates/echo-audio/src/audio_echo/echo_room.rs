//! A made-up voice and a made-up room for tests of the canceller. Nothing
//! here is recorded: every sample comes from a seeded generator.
use crate::audio_sample::pcm16;
use std::f64::consts::TAU;

const RATE: f64 = 16_000.0;

/// xorshift64*: the same samples on every machine.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let top = u32::try_from(self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 32).unwrap_or(0);
        f64::from(top) / f64::from(u32::MAX) * 2.0 - 1.0
    }

    /// A value from `low` to `high`.
    fn within(&mut self, low: f64, high: f64) -> f64 {
        low + f64::midpoint(self.next(), 1.0) * (high - low)
    }
}

/// A two-pole resonance: one formant.
#[derive(Default)]
struct Formant {
    a1: f64,
    a2: f64,
    y1: f64,
    y2: f64,
}

impl Formant {
    fn tune(&mut self, hz: f64, width_hz: f64) {
        let radius = (-std::f64::consts::PI * width_hz / RATE).exp();
        self.a1 = 2.0 * radius * (TAU * hz / RATE).cos();
        self.a2 = -radius * radius;
    }

    fn step(&mut self, input: f64) -> f64 {
        let output = input + self.a1 * self.y1 + self.a2 * self.y2;
        self.y2 = self.y1;
        self.y1 = output;
        output
    }
}

/// Speech-like audio: syllables of a pulsed, breathy source through three
/// formants that move from one syllable to the next, with pauses between.
/// The pitch glides and wavers as a voice's does, and one syllable in four is
/// unvoiced. `seed` picks the voice and what it says; `level` is the RMS it
/// is scaled to.
#[must_use]
pub fn voice(seed: u64, samples: usize, level: f64) -> Vec<i16> {
    let mut noise = Noise(seed | 1);
    let mut formants = [Formant::default(), Formant::default(), Formant::default()];
    let (mut raw, mut phase) = (Vec::with_capacity(samples), 0.0_f64);
    while raw.len() < samples {
        let voiced = noise.next() > -0.5;
        let (pitch, glide) = (noise.within(100.0, 210.0), noise.within(-0.25, 0.25));
        formants[0].tune(noise.within(300.0, 800.0), 90.0);
        formants[1].tune(noise.within(900.0, 2200.0), 120.0);
        formants[2].tune(noise.within(2400.0, 3300.0), 180.0);
        let length = noise.within(0.12, 0.32) * RATE;
        let (mut at, mut waver) = (0.0, 1.0);
        while at < length && raw.len() < samples {
            let step = pitch * (1.0 + glide * at / length) * waver / RATE;
            if phase + step >= 1.0 {
                waver = noise.within(0.97, 1.03);
            }
            phase = (phase + step).fract();
            let pulse = if phase < 0.1 { 1.0 } else { -0.11 };
            let source = if voiced {
                pulse + 0.15 * noise.next()
            } else {
                0.4 * noise.next()
            };
            let [first, second, third] = &mut formants;
            let sound = first.step(source) + 0.5 * second.step(source) + 0.25 * third.step(source);
            // 20 ms in and out of every syllable.
            let edge = (at.min(length - at) / (0.02 * RATE)).min(1.0);
            raw.push(sound * (0.5 - 0.5 * (edge * std::f64::consts::PI).cos()));
            at += 1.0;
        }
        let pause = if noise.next() > 0.8 {
            0.4
        } else {
            noise.within(0.03, 0.12)
        };
        let silence = raw.len() + pcm_len(pause * RATE);
        raw.resize(silence.min(samples), 0.0);
    }
    let power: f64 = raw.iter().map(|sample| sample * sample).sum();
    let scale = level / (power / RATE / seconds(samples)).sqrt().max(f64::MIN_POSITIVE);
    raw.iter().map(|sample| pcm16(sample * scale)).collect()
}

fn pcm_len(samples: f64) -> usize {
    usize::try_from(pcm16(samples.min(32_000.0))).unwrap_or(0)
}

fn seconds(samples: usize) -> f64 {
    f64::from(u32::try_from(samples).unwrap_or(u32::MAX)) / RATE
}

/// What a microphone in a room hears of `far` played by a speaker beside
/// it: the direct sound `delay` samples late and `gain` times as loud, then
/// 16 ms of decaying reflections. `seed` picks the room.
#[must_use]
pub fn echo(far: &[i16], delay: usize, gain: f64, seed: u64) -> Vec<i16> {
    let mut noise = Noise(seed | 1);
    let mut taps = vec![1.0];
    let mut fade = 0.5;
    for _ in 1..256 {
        taps.push(noise.next() * fade);
        fade *= 0.98;
    }
    (0..far.len())
        .map(|index| {
            let heard: f64 = taps
                .iter()
                .enumerate()
                .filter_map(|(tap, weight)| Some(f64::from(far[index.checked_sub(delay + tap)?]) * weight))
                .sum();
            pcm16(heard * gain)
        })
        .collect()
}

/// A quiet room's own sound at about `level` RMS.
#[must_use]
pub fn hiss(seed: u64, samples: usize, level: f64) -> Vec<i16> {
    let mut noise = Noise(seed | 1);
    // Uniform noise of amplitude `a` has an RMS of `a / sqrt(3)`.
    (0..samples).map(|_| pcm16(noise.next() * level * 1.732)).collect()
}

/// Sample by sample, saturating.
#[must_use]
pub fn mix(first: &[i16], second: &[i16]) -> Vec<i16> {
    first.iter().zip(second).map(|(a, b)| a.saturating_add(*b)).collect()
}

/// The energy of `samples` in dB, on a scale where one unit of RMS is 0 dB.
#[must_use]
pub fn level_db(samples: &[i16]) -> f64 {
    let power: f64 = samples.iter().map(|sample| f64::from(*sample).powi(2)).sum();
    10.0 * (power / seconds(samples.len()) / RATE).max(1.0).log10()
}

/// How alike two signals are, from -1 to 1, at the lag up to `reach` samples
/// by which `late` trails `clean` that makes them most alike.
#[must_use]
pub fn likeness(clean: &[i16], late: &[i16], reach: usize) -> f64 {
    let dot = |a: &[i16], b: &[i16]| -> f64 { a.iter().zip(b).map(|(x, y)| f64::from(*x) * f64::from(*y)).sum() };
    (0..=reach)
        .filter_map(|lag| {
            let late = late.get(lag..)?;
            let length = late.len().min(clean.len());
            let (clean, late) = (&clean[..length], &late[..length]);
            Some(dot(clean, late) / (dot(clean, clean) * dot(late, late)).sqrt().max(1.0))
        })
        .fold(-1.0, f64::max)
}
