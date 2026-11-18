//! Original quiet two-tone acknowledgement; no recorded or borrowed audio.
use std::f64::consts::TAU;

pub(super) const SAMPLES: u32 = 3840;
const TONE: u32 = SAMPLES / 2;
const FADE: u32 = 240;

pub(super) fn samples() -> Vec<i16> {
    (0..SAMPLES)
        .map(|index| {
            let position = index % TONE;
            let edge = position.min(TONE - 1 - position).min(FADE);
            let fade = (1.0 - (std::f64::consts::PI * f64::from(edge) / f64::from(FADE)).cos()) / 2.0;
            let frequency = if index < TONE { 750.0 } else { 1000.0 };
            pcm(983.0 * fade * (TAU * frequency * f64::from(position) / 24_000.0).sin())
        })
        .collect()
}

/// The synthesized value is finite and bounded to +/-983. Decode its rounded
/// IEEE magnitude instead of using an unchecked float-to-integer cast.
fn pcm(value: f64) -> i16 {
    let value = value.round();
    if value.abs() < 1.0 {
        return 0;
    }
    let bits = value.abs().to_bits();
    let exponent = (bits >> 52) & 0x7ff;
    let significand = (bits & ((1_u64 << 52) - 1)) | (1_u64 << 52);
    let magnitude = i16::try_from(significand >> (1075 - exponent)).unwrap_or(983);
    if value.is_sign_negative() {
        -magnitude
    } else {
        magnitude
    }
}
