//! T1: synthetic windows, 20 ms each at 24 kHz as the bridge writes them.
use super::*;
use crate::audio_sample::pcm16;
use std::f64::consts::TAU;

const RATE: u16 = 24_000;
const WINDOW: u32 = 480;

/// Window `index` of a sum of sinusoids given as (Hz, peak amplitude).
fn window(parts: &[(f64, f64)], index: u32) -> Vec<u8> {
    (index * WINDOW..(index + 1) * WINDOW)
        .flat_map(|sample| {
            let time = f64::from(sample) / f64::from(RATE);
            let value: f64 = parts
                .iter()
                .map(|(hertz, peak)| peak * (TAU * hertz * time).sin())
                .sum();
            pcm16(value).to_le_bytes()
        })
        .collect()
}

/// A voiced sound: harmonics of `pitch` falling 6 dB an octave, lifted
/// around each formant by a resonance 90 Hz wide.
fn voiced(pitch: f64, formants: [f64; 2]) -> Vec<(f64, f64)> {
    (1..=24)
        .map(|harmonic| {
            let hertz = pitch * f64::from(harmonic);
            let lift: f64 = formants
                .iter()
                .map(|formant| 1.0 / (1.0 + ((hertz - formant) / 90.0).powi(2)))
                .sum();
            (hertz, 12000.0 * lift / f64::from(harmonic))
        })
        .collect()
}

fn analyser() -> LipAnalyser {
    LipAnalyser::new(RATE, LipOptions::default())
}

fn settled(parts: &[(f64, f64)]) -> LipSample {
    let mut lips = analyser();
    (0..6).map(|index| lips.window(&window(parts, index))).last().unwrap()
}

#[test]
fn silence_and_sound_under_the_noise_floor_keep_the_mouth_shut() {
    let mut lips = analyser();
    for index in 0..10 {
        assert_eq!(lips.window(&[0; 960]), LipSample::default());
        // A 300 Hz hum of RMS 212 is under the default floor of 250.
        assert_eq!(lips.window(&window(&[(300.0, 300.0)], index)), LipSample::default());
    }
    assert!(lips.at_rest());
    assert_eq!(lips.window(&[]), LipSample::default());
}

#[test]
fn a_tone_burst_opens_within_two_windows_and_closes_within_three() {
    let mut lips = analyser();
    let burst = [(440.0, 8000.0)];
    let open: Vec<u8> = (0..10).map(|index| lips.window(&window(&burst, index)).open).collect();
    // RMS 5657 is past full gain: 70% of the way a window, never overshooting.
    assert_eq!(open, [70, 91, 98, 100, 100, 100, 100, 100, 100, 100]);
    assert!(!lips.at_rest());
    let closing: Vec<u8> = (0..4).map(|_| lips.window(&[0; 960]).open).collect();
    assert_eq!(closing, [60, 20, 0, 0]);
    assert!(lips.at_rest());
}

#[test]
fn openness_follows_loudness_between_the_floor_and_full_gain() {
    // RMS 1437: a quarter of the way from the floor to full is a half-open mouth.
    assert_eq!(settled(&[(440.0, 2033.0)]).open, 50);
    let options = LipOptions {
        floor: 1000,
        full: 2000,
        attack: 100,
        release: 100,
    };
    let mut lips = LipAnalyser::new(RATE, options);
    assert_eq!(lips.window(&window(&[(440.0, 8000.0)], 0)).open, 100);
    assert_eq!(lips.window(&window(&[(440.0, 1300.0)], 1)).open, 0);
}

#[test]
fn formant_pairs_classify_as_their_vowel() {
    assert_eq!(settled(&[(800.0, 6000.0), (1200.0, 3000.0)]).vowel, Some(Vowel::A));
    assert_eq!(settled(&[(300.0, 6000.0), (2500.0, 1500.0)]).vowel, Some(Vowel::I));
    assert_eq!(settled(&[(350.0, 6000.0), (1000.0, 2500.0)]).vowel, Some(Vowel::U));
    assert_eq!(settled(&[(500.0, 6000.0), (2000.0, 2000.0)]).vowel, Some(Vowel::E));
    assert_eq!(settled(&[(500.0, 6000.0), (900.0, 3000.0)]).vowel, Some(Vowel::O));
}

#[test]
fn voiced_harmonics_classify_by_their_formants() {
    for (vowel, formants) in [
        (Vowel::A, [800.0, 1200.0]),
        (Vowel::I, [300.0, 2500.0]),
        (Vowel::E, [500.0, 2000.0]),
        (Vowel::O, [500.0, 900.0]),
    ] {
        for pitch in [110.0, 200.0] {
            let sample = settled(&voiced(pitch, formants));
            assert_eq!(sample.vowel, Some(vowel), "{vowel:?} at {pitch} Hz");
            assert!(sample.open > 0);
        }
    }
}

#[test]
fn a_vowel_needs_two_windows_and_ends_with_the_sound() {
    let mut lips = analyser();
    let a = [(800.0, 6000.0), (1200.0, 3000.0)];
    let i = [(300.0, 6000.0), (2500.0, 1500.0)];
    assert_eq!(lips.window(&window(&a, 0)).vowel, None);
    assert_eq!(lips.window(&window(&a, 1)).vowel, Some(Vowel::A));
    // One window of another shape does not flicker the mouth.
    assert_eq!(lips.window(&window(&i, 2)).vowel, Some(Vowel::A));
    assert_eq!(lips.window(&window(&i, 3)).vowel, Some(Vowel::I));
    assert_eq!(lips.window(&[0; 960]).vowel, None);
}

#[test]
fn hiss_and_a_lone_tone_are_no_vowel() {
    // 6 kHz changes sign every other sample: a fricative, not a vowel.
    let hiss = settled(&[(6000.0, 6000.0)]);
    assert_eq!((hiss.open, hiss.vowel), (91, None));
    // One partial between the centres is near none of them.
    assert_eq!(settled(&[(650.0, 6000.0)]).vowel, None);
}
