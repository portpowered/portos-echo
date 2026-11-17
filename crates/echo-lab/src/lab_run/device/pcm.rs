use std::path::{Path, PathBuf};

use super::super::sim::observers;

pub fn wav(pcm: &[u8]) -> Result<Vec<u8>, String> {
    if pcm.is_empty() || pcm.len() > 4096 || !pcm.len().is_multiple_of(2) {
        return Err("device say: expected 1..2048 complete PCM samples".into());
    }
    let size = u32::try_from(pcm.len()).map_err(|_| "device say: PCM size overflow")?;
    let mut bytes = Vec::with_capacity(pcm.len() + 44);
    bytes.extend(b"RIFF");
    bytes.extend((size + 36).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(16000_u32.to_le_bytes());
    bytes.extend(32000_u32.to_le_bytes());
    bytes.extend(2_u16.to_le_bytes());
    bytes.extend(16_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(size.to_le_bytes());
    bytes.extend(pcm);
    Ok(bytes)
}

pub fn adapt(path: &Path, root: &Path, step: usize) -> Result<PathBuf, String> {
    if path.extension().is_none_or(|ext| ext != "pcm") {
        return Ok(path.to_owned());
    }
    let contents = wav(&observers::bytes(path, 4096, "device say")?)?;
    let destination = root.join(format!("utterance-{step}.wav"));
    std::fs::write(&destination, contents).map_err(|_| "device say: cannot write run-local WAV")?;
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_pcm_preserves_signed_samples_and_format() {
        let pcm: Vec<_> = [i16::MIN, -1, 0, 1, i16::MAX]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect();
        let bytes = wav(&pcm).unwrap();
        assert_eq!(&bytes[44..], pcm);
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(&bytes[22..24], 1_u16.to_le_bytes());
        assert_eq!(&bytes[24..28], 16000_u32.to_le_bytes());
        assert_eq!(&bytes[34..36], 16_u16.to_le_bytes());
        assert_eq!(&bytes[40..44], 10_u32.to_le_bytes());
        assert_eq!(wav(&vec![0; 4096]).unwrap().len(), 4140);
        for invalid in [vec![], vec![0], vec![0; 4098]] {
            assert!(wav(&invalid).is_err());
        }
    }
}
