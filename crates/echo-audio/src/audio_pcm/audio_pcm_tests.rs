use super::*;
use std::collections::BTreeMap;
use std::f64::consts::TAU;
use std::io::Write;

fn format(rate: u32, channels: u16) -> AudioFormat {
    AudioFormat {
        rate,
        width: 2,
        channels,
        extra: BTreeMap::default(),
    }
}

fn convert(rate: u32, target: u32, channels: u16, input: &[i16]) -> Vec<i16> {
    let mut converter = PcmResampler::new(&format(rate, channels), target).unwrap();
    let mut scratch = vec![0; converter.output_capacity()];
    let mut output = Vec::new();
    // Irregular caller chunks independent of the internal 20 ms block.
    for chunk in input.chunks(137 * usize::from(channels)) {
        let mut offset = 0;
        while offset < chunk.len() {
            let progress = converter.process(&chunk[offset..], &mut scratch).unwrap();
            assert!(progress.consumed > 0);
            offset += progress.consumed;
            output.extend_from_slice(&scratch[..progress.written]);
        }
    }
    loop {
        let count = converter.finish(&mut scratch).unwrap();
        if count == 0 {
            break;
        }
        output.extend_from_slice(&scratch[..count]);
    }
    assert_eq!(converter.finish(&mut scratch).unwrap(), 0);
    output
}

fn sine(rate: u32, length: u32) -> Vec<i16> {
    (0..length)
        .map(|n| pcm16(16384.0 * (TAU * 1000.0 * f64::from(n) / f64::from(rate)).sin()))
        .collect()
}

#[test]
fn all_supported_rate_pairs_have_exact_stream_lengths() {
    for rate in [16_000, 22_050, 24_000, 48_000] {
        // A non-frame-aligned finite duration also tests rounding on finish.
        let length = rate + 73;
        let input = sine(rate, length);
        for target in [16_000, 22_050, 24_000, 48_000] {
            let output = convert(rate, target, 1, &input);
            let expected = u64::from(length) * u64::from(target) / u64::from(rate);
            assert_eq!(u64::try_from(output.len()).unwrap(), expected);
            if target == rate {
                assert_eq!(output, input);
            }
            let stereo: Vec<_> = input.iter().flat_map(|&sample| [sample, sample]).collect();
            assert_eq!(convert(rate, target, 2, &stereo), output);
            let opposite: Vec<_> = input.iter().flat_map(|&sample| [sample, -sample]).collect();
            assert_eq!(convert(rate, target, 2, &opposite), vec![0; output.len()]);
        }
    }
}

fn rms(input: &[i16]) -> f64 {
    let energy: f64 = input.iter().map(|&sample| f64::from(sample).powi(2)).sum();
    (energy / f64::from(u32::try_from(input.len()).unwrap())).sqrt()
}

fn frequency(input: &[i16], rate: u32) -> f64 {
    let crossings: Vec<_> = input
        .windows(2)
        .enumerate()
        .filter_map(|(index, pair)| {
            if pair[0] <= 0 && pair[1] > 0 {
                let fraction = -f64::from(pair[0]) / (f64::from(pair[1]) - f64::from(pair[0]));
                Some(f64::from(u32::try_from(index).unwrap()) + fraction)
            } else {
                None
            }
        })
        .collect();
    f64::from(u32::try_from(crossings.len() - 1).unwrap()) * f64::from(rate)
        / (crossings.last().unwrap() - crossings[0])
}

#[test]
fn sine_roundtrip_preserves_frequency_and_rms() {
    let input = sine(16_000, 16_000);
    let up = convert(16_000, 48_000, 1, &input);
    assert_eq!(up.len(), 48_000);
    let output = convert(48_000, 16_000, 1, &up);
    assert_eq!(output.len(), 16_000);
    // Fixed 20 ms edge exclusion, independent of measured values.
    let measured = &output[320..15_680];
    let frequency_hz = frequency(measured, 16_000);
    let frequency_error_hz = (frequency_hz - 1000.0).abs();
    let rms_delta_db = 20.0 * (rms(measured) / rms(&input[320..15_680])).log10();
    writeln!(
        std::io::stdout(),
        "frequency_hz={frequency_hz:.6} frequency_error_hz={frequency_error_hz:.6} rms_delta_db={rms_delta_db:.6}"
    )
    .unwrap();
    assert!(frequency_error_hz <= 1.0);
    assert!(rms_delta_db.abs() <= 0.5);
}

#[test]
fn exact_identity_downmix_and_quantization() {
    assert_eq!(
        convert(24_000, 24_000, 1, &[i16::MIN, -1, 0, 1, i16::MAX]),
        [i16::MIN, -1, 0, 1, i16::MAX]
    );
    assert_eq!(
        convert(24_000, 24_000, 2, &[32767, 32767, -32768, -32768, -3, 2, 3, -2]),
        [32767, -32768, 0, 0]
    );
    for value in i16::MIN..=i16::MAX {
        assert_eq!(pcm16(f64::from(value)), value);
    }
    assert_eq!(pcm16(0.5), 1);
    assert_eq!(pcm16(-0.5), -1);
    assert_eq!(pcm16(1e10), i16::MAX);
    assert_eq!(pcm16(-1e10), i16::MIN);
    assert_eq!(pcm16(f64::NAN), 0);
}

#[test]
fn rejects_unsupported_formats() {
    for invalid in [
        format(8000, 1),
        format(48000, 3),
        AudioFormat {
            width: 1,
            ..format(48000, 1)
        },
    ] {
        assert!(matches!(PcmResampler::new(&invalid, 48000), Err(AudioError::Format)));
    }
    assert!(matches!(
        PcmResampler::new(&format(48000, 1), 8000),
        Err(AudioError::Format)
    ));
}

#[test]
fn validation_consumes_nothing_and_reset_discards_old_audio() {
    let mut converter = PcmResampler::new(&format(16000, 2), 48000).unwrap();
    let mut out = vec![0; converter.output_capacity()];
    assert!(matches!(converter.process(&[1], &mut out), Err(AudioError::Input)));
    assert!(matches!(converter.process(&[1, 1], &mut []), Err(AudioError::Output)));
    assert!(matches!(converter.finish(&mut []), Err(AudioError::Output)));
    assert_eq!(
        converter.process(&[2000; 30], &mut out).unwrap(),
        SampleProgress {
            consumed: 30,
            written: 0
        }
    );
    converter.reset();
    assert_eq!(converter.finish(&mut out).unwrap(), 0);
    assert!(matches!(
        converter.process(&[0, 0], &mut out),
        Err(AudioError::Finished)
    ));
    converter.reset();
    assert_eq!(converter.process(&[0; 640], &mut out).unwrap().consumed, 640);
    let count = converter.finish(&mut out).unwrap();
    assert_eq!(&out[..count], vec![0; count]);
}

#[test]
fn empty_and_sub_block_streams_finish_exactly() {
    // Very short streams still drain delay rather than returning premature zero.
    assert_eq!(convert(16000, 48000, 1, &[1]).len(), 3);
    assert_eq!(convert(48000, 16000, 1, &[1]).len(), 0);
    assert_eq!(convert(16000, 48000, 1, &[]), Vec::<i16>::new());
}
