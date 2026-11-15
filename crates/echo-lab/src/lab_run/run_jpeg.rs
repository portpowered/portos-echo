//! Structural JPEG validation only: entropy bytes are deliberately not decoded.

pub fn validate(bytes: &[u8]) -> Result<(), &'static str> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return Err("JPEG missing SOI");
    }
    if !bytes.ends_with(&[0xff, 0xd9]) {
        return Err("JPEG missing EOI");
    }
    let body = &bytes[..bytes.len() - 2];
    let (mut offset, mut frames, mut quantization, mut huffman) = (2, 0, false, false);
    while offset < body.len() {
        let (marker, payload) = segment(body, &mut offset)?;
        match marker {
            0xc0..=0xc2 => {
                frames += 1;
                if frames != 1 {
                    return Err("JPEG duplicate SOF");
                }
                frame(payload)?;
            }
            0xdb | 0xc4 => {
                if payload.is_empty() {
                    return Err("JPEG empty table");
                }
                quantization |= marker == 0xdb;
                huffman |= marker == 0xc4;
            }
            0xda => {
                if frames == 0 {
                    return Err("JPEG missing SOF");
                }
                if !quantization || !huffman {
                    return Err("JPEG missing DQT or DHT");
                }
                if payload.is_empty()
                    || !matches!(payload[0], 1 | 3)
                    || payload.len() != 4 + 2 * usize::from(payload[0])
                {
                    return Err("JPEG invalid SOS");
                }
                return if offset < body.len() {
                    Ok(())
                } else {
                    Err("JPEG no data")
                };
            }
            0xe0..=0xef | 0xfe | 0xdd => {}
            _ => return Err("JPEG unsupported marker"),
        }
    }
    Err("JPEG no SOS")
}

fn segment<'a>(bytes: &'a [u8], offset: &mut usize) -> Result<(u8, &'a [u8]), &'static str> {
    if bytes.get(*offset) != Some(&0xff) {
        return Err("JPEG invalid marker");
    }
    while bytes.get(*offset) == Some(&0xff) {
        *offset += 1;
    }
    let marker = *bytes.get(*offset).ok_or("JPEG truncated segment")?;
    *offset += 1;
    let length = bytes.get(*offset..*offset + 2).ok_or("JPEG truncated segment")?;
    let length = usize::from(u16::from_be_bytes([length[0], length[1]]));
    if length < 2 {
        return Err("JPEG invalid segment length");
    }
    let end = *offset + length;
    let payload = bytes.get(*offset + 2..end).ok_or("JPEG truncated segment")?;
    *offset = end;
    Ok((marker, payload))
}

fn frame(payload: &[u8]) -> Result<(), &'static str> {
    if payload.len() < 6 {
        return Err("JPEG truncated SOF");
    }
    if payload[1..3] == [0, 0] || payload[3..5] == [0, 0] {
        return Err("JPEG zero dimension");
    }
    if !matches!(payload[5], 1 | 3) {
        return Err("JPEG unsupported component count");
    }
    if payload.len() != 6 + 3 * usize::from(payload[5]) {
        return Err("JPEG invalid SOF length");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate;

    // Hand-assembled marker segments; entropy is intentionally garbage.
    fn jpeg(sof: &[u8], tables: bool, scan: bool, entropy: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0xff, 0xd8];
        if tables {
            append(&mut bytes, 0xdb, &[0; 65]);
            append(&mut bytes, 0xc4, &[0; 17]);
        }
        if !sof.is_empty() {
            append(&mut bytes, 0xc0, sof);
        }
        if scan {
            append(&mut bytes, 0xda, &[1, 1, 0, 0, 63, 0]);
        }
        bytes.extend_from_slice(entropy);
        bytes.extend_from_slice(&[0xff, 0xd9]);
        bytes
    }

    fn append(bytes: &mut Vec<u8>, marker: u8, payload: &[u8]) {
        bytes.extend_from_slice(&[0xff, marker]);
        bytes.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
        bytes.extend_from_slice(payload);
    }

    const SOF: &[u8] = &[8, 0, 1, 0, 1, 1, 1, 0x11, 0];

    #[test]
    fn valid_fixture_and_structural_garbage_entropy_pass() {
        assert_eq!(
            validate(include_bytes!("../../../../tests/goldens/lab-frame.jpg")),
            Ok(())
        );
        for marker in [0xc0, 0xc1, 0xc2] {
            let mut bytes = jpeg(SOF, true, true, b"garbage entropy");
            let position = bytes.windows(2).position(|w| w == [0xff, 0xc0]).unwrap();
            bytes[position + 1] = marker;
            assert_eq!(validate(&bytes), Ok(()));
        }
        let rgb = [8, 0, 2, 0, 3, 3, 1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0];
        assert_eq!(validate(&jpeg(&rgb, true, true, &[0])), Ok(()));
    }

    #[test]
    fn missing_structure_has_named_failures() {
        for (bytes, reason) in [
            (vec![0xff, 0xd9], "JPEG missing SOI"),
            (vec![0xff, 0xd8], "JPEG missing EOI"),
            (jpeg(&[], true, true, &[0]), "JPEG missing SOF"),
            (jpeg(SOF, false, true, &[0]), "JPEG missing DQT or DHT"),
            (jpeg(SOF, true, false, &[]), "JPEG no SOS"),
            (jpeg(SOF, true, true, &[]), "JPEG no data"),
            (jpeg(&[8], true, true, &[0]), "JPEG truncated SOF"),
        ] {
            assert_eq!(validate(&bytes), Err(reason));
        }
        for index in [2, 4, 5] {
            let mut sof = SOF.to_vec();
            sof[index] = 0;
            let reason = if index == 5 {
                "JPEG unsupported component count"
            } else {
                "JPEG zero dimension"
            };
            assert_eq!(validate(&jpeg(&sof, true, true, &[0])), Err(reason));
        }
    }

    #[test]
    fn malformed_segments_and_duplicate_frames_fail() {
        for (segment, reason) in [
            (vec![0xff], "JPEG truncated segment"),
            (vec![0xff, 0xdb, 0], "JPEG truncated segment"),
            (vec![0xff, 0xdb, 0, 9, 0], "JPEG truncated segment"),
            (vec![0xff, 0xdb, 0, 1], "JPEG invalid segment length"),
            (vec![0xff, 0xdb, 0, 2], "JPEG empty table"),
            (vec![0], "JPEG invalid marker"),
            (vec![0xff, 0xd8, 0, 2], "JPEG unsupported marker"),
        ] {
            let mut bytes = vec![0xff, 0xd8];
            bytes.extend(segment);
            bytes.extend([0xff, 0xd9]);
            assert_eq!(validate(&bytes), Err(reason));
        }
        let mut bytes = jpeg(SOF, true, false, &[]);
        bytes.truncate(bytes.len() - 2);
        append(&mut bytes, 0xc0, SOF);
        bytes.extend([0xff, 0xd9]);
        assert_eq!(validate(&bytes), Err("JPEG duplicate SOF"));
    }

    #[test]
    fn each_table_is_required_and_frame_and_scan_lengths_are_checked() {
        for marker in [0xdb, 0xc4] {
            let mut bytes = vec![0xff, 0xd8];
            append(&mut bytes, marker, &[0]);
            append(&mut bytes, 0xc0, SOF);
            append(&mut bytes, 0xda, &[1, 1, 0, 0, 63, 0]);
            bytes.extend([0, 0xff, 0xd9]);
            assert_eq!(validate(&bytes), Err("JPEG missing DQT or DHT"));
        }
        assert_eq!(
            validate(&jpeg(&SOF[..8], true, true, &[0])),
            Err("JPEG invalid SOF length")
        );
        for scan in [&[][..], &[0, 0, 63, 0], &[1, 1, 0]] {
            let mut bytes = jpeg(SOF, true, false, &[]);
            bytes.truncate(bytes.len() - 2);
            append(&mut bytes, 0xda, scan);
            bytes.extend([0, 0xff, 0xd9]);
            assert_eq!(validate(&bytes), Err("JPEG invalid SOS"));
        }
    }
}
