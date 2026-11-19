use std::{
    io::Write,
    path::{Path, PathBuf},
};

use super::super::sim::observers;

pub const INVALID_CLIP: &str = "device: invalid wake clip; expected 16-bit PCM mono WAV at 16000 or 48000 Hz, 0.5-10 s";

// Authored persistent utterances are distinct speech, rather than the sim PCM
// markers or a single caller-supplied clip repeated for every turn.
pub fn persistent_input(
    value: &str,
    scenario: &str,
    options: &super::super::run_options::Options,
) -> Result<Option<super::super::Step>, String> {
    if options.sim || options.bridge_config.is_none() {
        return Ok(None);
    }
    let scenario = crate::lab_validate::load(scenario)?;
    session_text(value, &scenario, options.speech.is_some(), options.wake_clip.is_some())
}

fn session_text(
    value: &str,
    scenario: &serde_json::Value,
    speech: bool,
    clip: bool,
) -> Result<Option<super::super::Step>, String> {
    if !matches!(
        scenario["id"].as_str(),
        Some("session-two-turns" | "session-idle-timeout")
    ) {
        return Ok(None);
    }
    if clip {
        return Err(
            "device: session utterances require speech synthesis; --wake-clip would replace authored text".into(),
        );
    }
    if !speech || value.trim().is_empty() {
        return Err("device: session utterances require --speech-endpoint and --speech-model".into());
    }
    Ok(Some(super::super::Step::SayText(value.to_owned())))
}

pub struct Clip {
    bytes: Vec<u8>,
    pub metadata: serde_json::Value,
}

impl Clip {
    pub fn load(path: &str, python: &str) -> Result<Self, String> {
        let output = super::super::run_command::controlled(
            Path::new(python),
            &[
                "-c".into(),
                include_str!("wake_clip.py").into(),
                path.into(),
                env!("CARGO_MANIFEST_DIR").into(),
            ],
            std::time::Instant::now() + std::time::Duration::from_secs(5),
            None,
        )
        .map_err(|_| INVALID_CLIP)?;
        let split = output.iter().position(|b| *b == b'\n').ok_or(INVALID_CLIP)?;
        let metadata = serde_json::from_slice(&output[..split]).map_err(|_| INVALID_CLIP)?;
        Ok(Self {
            bytes: output[split + 1..].to_vec(),
            metadata,
        })
    }

    pub fn stage(&self, root: &Path) -> Result<PathBuf, String> {
        let path = root.join("configured.private/wake.wav");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(&path)
            .and_then(|mut file| file.write_all(&self.bytes))
            .map_err(|_| "device: cannot stage wake clip")?;
        Ok(path)
    }

    fn source(&self) -> serde_json::Value {
        serde_json::json!({"kind":"wake_clip","sha256":self.metadata["sha256"]})
    }
}

pub struct Standalone<'a> {
    options: &'a super::super::run_options::Options,
    bench: super::super::run_device::Device<'a>,
    plays: Vec<serde_json::Value>,
}

impl<'a> Standalone<'a> {
    pub fn new(options: &'a super::super::run_options::Options) -> Self {
        Self {
            options,
            bench: super::super::run_device::Device::new(options),
            plays: Vec::new(),
        }
    }
}

impl super::super::Driver for Standalone<'_> {
    fn execute(
        &mut self,
        step: &super::super::Step,
        index: usize,
        next: Option<&super::super::Step>,
        artifacts: &mut super::super::run_report::Artifacts,
    ) -> Result<(), String> {
        let super::super::Step::SayFile(_) = step else {
            return self.bench.execute(step, index, next, artifacts);
        };
        let clip = self.options.wake_clip.as_ref().ok_or(INVALID_CLIP)?;
        let directory = artifacts.root.join("configured.private");
        if !directory.exists() {
            std::fs::create_dir(&directory).map_err(|_| "device: cannot stage wake clip")?;
        }
        let path = clip.stage(&artifacts.root)?;
        self.bench
            .execute(&super::super::Step::SayFile(path), index, next, artifacts)?;
        self.plays
            .push(serde_json::json!({"step":index,"iteration":0,"source":clip.source()}));
        Ok(())
    }

    fn close(&mut self, artifacts: &mut super::super::run_report::Artifacts) -> Result<(), String> {
        let result = self.bench.close(artifacts);
        let directory = artifacts.root.join("configured.private");
        let cleanup = if directory.exists() {
            std::fs::remove_dir_all(directory).map_err(|_| "device: private cleanup failed".into())
        } else {
            Ok(())
        };
        if let Some(clip) = &self.options.wake_clip {
            artifacts.m1 = Some(serde_json::json!({"wake_clip":clip.metadata,"plays":self.plays,
            "ready_before_first_wake":0,"streamed_wakes":[],"bridge_restarts":0,"hassmic_host_restarts":0,"mute_status":"blocked"}));
        }
        super::super::combine(result, cleanup)
    }
}

impl super::Configured<'_> {
    pub(super) fn play(
        &mut self,
        path: &Path,
        index: usize,
        next: Option<&super::super::Step>,
        artifacts: &mut super::super::run_report::Artifacts,
    ) -> Result<(), String> {
        use super::super::{Driver, Step};
        let clip = self.options.wake_clip.as_ref();
        let path = clip.map_or_else(
            || adapt(path, &artifacts.root, index),
            |clip| clip.stage(&artifacts.root),
        )?;
        if !self.session || self.wire_replies == 0 {
            self.state.arm_wake()?;
        }
        if self.session {
            self.bench.session_input(&Step::SayFile(path), index, artifacts)?;
        } else {
            self.bench.execute(&Step::SayFile(path), index, next, artifacts)?;
        }
        let play_index = self.state.plays.len();
        if let Some(clip) = clip {
            self.state.clip = Some(clip.metadata.clone());
            self.state
                .plays
                .push(serde_json::json!({"step":index,"iteration":self.iteration,"source":clip.source()}));
        }
        if !self.state.muted() {
            self.wait_played(index)?;
            if let Some(clip) = clip.filter(|_| !self.session) {
                let wake = self.state.wakes.last_mut().ok_or("device: missing wake")?;
                wake["source"] = clip.source();
                wake["play_index"] = play_index.into();
            }
        }
        Ok(())
    }

    pub(super) fn play_text(
        &mut self,
        text: &str,
        index: usize,
        artifacts: &mut super::super::run_report::Artifacts,
    ) -> Result<(), String> {
        if self.wire_replies == 0 {
            self.state.arm_wake()?;
        }
        self.bench
            .session_input(&super::super::Step::SayText(text.to_owned()), index, artifacts)?;
        self.wait_played(index)
    }

    fn wait_played(&mut self, index: usize) -> Result<(), String> {
        if let Some(wire) = self.wire.as_mut() {
            self.executor.block_on(super::runtime::controlled(
                wire.wait_reply(1, self.wire_replies + 1),
                self.options.cancel.as_deref(),
            ))?;
            self.wire_replies += 1;
        }
        self.reply_observed(index)
    }

    fn reply_observed(&mut self, index: usize) -> Result<(), String> {
        self.executor.block_on(super::runtime::controlled(
            async {
                if self.session {
                    self.state.session_reply(self.wire_replies).await
                } else {
                    self.state.complete(index).await
                }
            },
            self.options.cancel.as_deref(),
        ))
    }
}

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
    fn device_session_inputs_preserve_authored_distinct_speech_and_refuse_clip_substitution() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/scenarios");
        for id in ["session-two-turns", "session-idle-timeout"] {
            let scenario = crate::lab_validate::load(root.join(format!("{id}.yaml")).to_str().unwrap()).unwrap();
            let mut utterances = Vec::new();
            for step in scenario["steps"].as_array().unwrap() {
                let Some(value) = step["say"].as_str() else { continue };
                let super::super::super::Step::SayText(text) =
                    session_text(value, &scenario, true, false).unwrap().unwrap()
                else {
                    panic!("session input must carry speech");
                };
                utterances.push(text);
                assert_eq!(
                    session_text(value, &scenario, false, false).err().unwrap(),
                    "device: session utterances require --speech-endpoint and --speech-model"
                );
                assert_eq!(
                    session_text(value, &scenario, true, true).err().unwrap(),
                    "device: session utterances require speech synthesis; --wake-clip would replace authored text"
                );
            }
            let expected = if id == "session-two-turns" {
                vec!["Computer, what time is it?", "Computer, what day is it?"]
            } else {
                vec!["Computer, what time is it?"]
            };
            assert_eq!(utterances, expected);
        }
        assert!(
            session_text(
                "utterance.wav",
                &serde_json::json!({"id":"m1-wake-stream"}),
                false,
                true
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn wake_clip_staging_uses_snapshot_instead_of_caller_or_previous_copy() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("configured.private")).unwrap();
        let clip = Clip {
            bytes: b"validated snapshot".to_vec(),
            metadata: serde_json::json!({}),
        };
        let path = clip.stage(root.path()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"validated snapshot");
        std::fs::write(&path, b"changed staging file").unwrap();
        clip.stage(root.path()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"validated snapshot");
    }

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
