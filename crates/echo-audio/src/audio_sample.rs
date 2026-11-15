/// Round to nearest, ties away from zero, then saturate to signed PCM16.
/// Extract the integral magnitude from IEEE-754 to avoid unchecked float casts.
pub(crate) fn pcm16(value: f64) -> i16 {
    let rounded = value.round().clamp(-32768.0, 32767.0);
    if rounded.abs() < 1.0 || rounded.is_nan() {
        return 0;
    }
    let bits = rounded.abs().to_bits();
    let exponent = (bits >> 52) & 0x7ff;
    let significand = (bits & ((1_u64 << 52) - 1)) | (1_u64 << 52);
    let magnitude = i32::try_from(significand >> (1075 - exponent)).unwrap_or(32768);
    let signed = if rounded.is_sign_negative() {
        -magnitude
    } else {
        magnitude
    };
    i16::try_from(signed).unwrap_or(if signed < 0 { i16::MIN } else { i16::MAX })
}
