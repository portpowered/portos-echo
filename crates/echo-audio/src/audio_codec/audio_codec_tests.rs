use super::*;
use crate::audio_sample::pcm16;
use std::f64::consts::TAU;
use std::io::Write;

#[test]
fn chirp_roundtrip_has_fifty_frames_and_speech_band_snr() {
    let reference: Vec<_> = (0..48_000)
        .map(|n| {
            let time = f64::from(n) / 48_000.0;
            let envelope = 0.35 + 0.1 * (TAU * 2.0 * time).sin();
            pcm16(32767.0 * envelope * (TAU * (300.0 * time + 1550.0 * time * time)).sin())
        })
        .collect();
    let mut encoder = OpusEncoder::new().unwrap();
    let lookahead = usize::try_from(encoder.lookahead().unwrap()).unwrap();
    let mut decoder = OpusDecoder::new().unwrap();
    let mut packet = [0; PACKET];
    let mut reconstructed = Vec::new();
    let mut packet_count = 0;
    for frame in reference.as_chunks::<FRAME>().0 {
        let length = encoder.encode(frame, &mut packet).unwrap();
        assert_eq!(opus::packet::get_nb_samples(&packet[..length], 48_000).unwrap(), FRAME);
        let mut output = [0; FRAME];
        assert_eq!(decoder.decode(&packet[..length], &mut output).unwrap(), FRAME);
        reconstructed.extend_from_slice(&output);
        packet_count += 1;
    }
    assert_eq!(packet_count, 50);
    assert_eq!(reconstructed.len(), 48_000);
    let compared_samples = 48_000 - lookahead;
    assert!(compared_samples > 0);
    let (energy, noise) = reference[..compared_samples]
        .iter()
        .zip(&reconstructed[lookahead..])
        .fold((0.0, 0.0), |(energy, noise), (&a, &b)| {
            (
                energy + f64::from(a).powi(2),
                noise + (f64::from(a) - f64::from(b)).powi(2),
            )
        });
    let snr_db = 10.0 * (energy / noise).log10();
    writeln!(std::io::stdout(),
        "snr_db={snr_db:.6} lookahead={lookahead} compared_samples={compared_samples} packet_count={packet_count} decoded_samples={}",
        reconstructed.len()
    ).unwrap();
    assert!(snr_db > 20.0);
}

#[test]
fn invalid_arguments_preserve_encoder_state() {
    let mut encoder = OpusEncoder::new().unwrap();
    let mut packet = [0; PACKET];
    assert!(matches!(encoder.encode(&[], &mut packet), Err(AudioError::Frame)));
    assert!(matches!(encoder.encode(&[0; 959], &mut packet), Err(AudioError::Frame)));
    assert!(matches!(encoder.encode(&[0; 961], &mut packet), Err(AudioError::Frame)));
    assert!(matches!(encoder.encode(&[0; FRAME], &mut []), Err(AudioError::Output)));
    let size = encoder.encode(&[0; FRAME], &mut packet).unwrap();
    let mut fresh = OpusEncoder::new().unwrap();
    let mut expected = [0; PACKET];
    assert_eq!(fresh.encode(&[0; FRAME], &mut expected).unwrap(), size);
    assert_eq!(packet[..size], expected[..size]);
}

#[test]
fn invalid_arguments_preserve_decoder_state() {
    let mut encoder = OpusEncoder::new().unwrap();
    let mut packet = [0; PACKET];
    let size = encoder.encode(&[0; FRAME], &mut packet).unwrap();
    let mut decoder = OpusDecoder::new().unwrap();
    let mut output = [0; FRAME];
    for invalid in [&[][..], &[0; PACKET + 1][..], &[3][..], &[0][..]] {
        assert!(matches!(decoder.decode(invalid, &mut output), Err(AudioError::Packet)));
    }
    assert!(matches!(
        decoder.decode(&packet[..size], &mut []),
        Err(AudioError::Output)
    ));
    assert_eq!(decoder.decode(&packet[..size], &mut output).unwrap(), FRAME);
    let mut fresh = OpusDecoder::new().unwrap();
    let mut expected = [0; FRAME];
    fresh.decode(&packet[..size], &mut expected).unwrap();
    assert_eq!(output, expected);
}
