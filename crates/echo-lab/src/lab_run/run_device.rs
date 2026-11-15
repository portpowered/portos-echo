use std::path::Path;
use std::time::Instant;

use super::run_command::{argv, bytes, helper, json};
use super::run_options::Options;
use super::run_report::Artifacts;
use super::{Driver, Step};

pub struct Device<'a> {
    options: &'a Options,
    hold: Option<(String, String)>,
    opened: Instant,
}

impl<'a> Device<'a> {
    pub fn new(options: &'a Options) -> Self {
        Self {
            options,
            hold: None,
            opened: Instant::now(),
        }
    }

    fn firmware(&self) -> Result<(), String> {
        let args = ["shell", "getprop", "ro.build.version.incremental"].map(str::to_owned);
        let output = bytes(&self.options.adb, &args).map_err(|_| "firmware: explicit adb read failed")?;
        let build = std::str::from_utf8(&output)
            .map_err(|_| "firmware: malformed build")?
            .trim();
        if !include_str!("../../data/probed-builds.txt")
            .lines()
            .any(|known| known == build)
        {
            return Err("firmware: build is not in probed-builds.txt; coordinator probe required".into());
        }
        Ok(())
    }

    fn audio_allowed(&self) -> Result<(), String> {
        let clock = helper(&self.options.python, "clock", &[]).map_err(|_| "clock: local time unavailable")?;
        match clock["hour"].as_u64() {
            Some(9..=23) => Ok(()),
            Some(0..=8) => Err("quiet hours: audio refused before 09:00 local time".into()),
            _ => Err("clock: invalid local hour".into()),
        }
    }

    fn open(&mut self, artifacts: &mut Artifacts) -> Result<(), String> {
        if self.hold.is_some() {
            // Rotation is a separate retained story. Until then fail closed
            // early enough to leave the 30-second release deadline below 11m.
            if self.opened.elapsed().as_secs() >= 450 {
                return Err("hold: batch limit reached; rotation not yet implemented".into());
            }
            return Ok(());
        }
        self.audio_allowed()?;
        self.opened = Instant::now();
        let result = json(&self.options.port, &argv("hold", &[("{ring}", &self.options.ring)])?)
            .map_err(|_| "hold: explicit port command failed")?;
        let hold = result["holdId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("hold: missing holdId")?;
        // Retain the id before checking other fields so malformed holds can be released.
        self.hold = Some((hold.to_owned(), String::new()));
        let lease = result["lease"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("hold: missing lease")?;
        self.hold = Some((hold.to_owned(), lease.to_owned()));
        if result["listening"] != true || result["talking"] != true {
            return Err("hold: camera must be listening and talking".into());
        }
        artifacts.event("hold_opened", None, None)?;
        let path = artifacts.root.join("alignment.wav");
        self.play("chirp", &[], &path)?;
        Ok(())
    }

    fn play(&self, operation: &str, extra: &[(&str, &str)], path: &Path) -> Result<(), String> {
        let (_, lease) = self.hold.as_ref().ok_or("play: no held session")?;
        let output = path.to_str().ok_or("play: output path is not UTF-8")?;
        let mut values = vec![
            ("{ring}", self.options.ring.as_str()),
            ("{lease}", lease),
            ("{output}", output),
        ];
        values.extend_from_slice(extra);
        let args = argv(operation, &values)?;
        self.audio_allowed()?;
        let result = json(&self.options.port, &args).map_err(|_| "play: port play-and-record failed")?;
        if result["held"] != true || !path.is_file() {
            return Err("play: missing held recording".into());
        }
        let mut trim = vec![output.to_owned()];
        for key in ["playStartedAt", "playEndedAt", "recordStartedAt", "recordEndedAt"] {
            trim.push(
                result[key]
                    .as_str()
                    .ok_or("play: missing capture timestamps")?
                    .to_owned(),
            );
        }
        let reply = helper(&self.options.python, "reply", &trim).map_err(|_| "play: invalid reply WAV or timing")?;
        if reply["reply_frames"].as_u64().is_none_or(|frames| frames == 0) {
            return Err("play: no post-playback reply audio".into());
        }
        Ok(())
    }
}

impl Driver for Device<'_> {
    fn execute(&mut self, step: &Step, index: usize, artifacts: &mut Artifacts) -> Result<(), String> {
        self.firmware()?;
        match step {
            Step::SayFile(path) => {
                self.open(artifacts)?;
                let file = path.to_str().ok_or("say: file path is not UTF-8")?;
                self.play(
                    "file",
                    &[("{file}", file)],
                    &artifacts.root.join(format!("reply-{index}.wav")),
                )
            }
            Step::SayText(text) => {
                self.open(artifacts)?;
                let (endpoint, model) = self
                    .options
                    .speech
                    .as_ref()
                    .ok_or("say: missing speech configuration")?;
                self.play(
                    "text",
                    &[("{text}", text), ("{endpoint}", endpoint), ("{model}", model)],
                    &artifacts.root.join(format!("reply-{index}.wav")),
                )
            }
            Step::Unsupported => Err("step: capability is not implemented by device driver".into()),
        }
    }

    fn close(&mut self, artifacts: &mut Artifacts) -> Result<(), String> {
        let Some((hold, _)) = self.hold.take() else {
            return Ok(());
        };
        json(&self.options.port, &argv("release", &[("{hold}", &hold)])?)
            .map_err(|_| "cleanup: port session release failed")?;
        artifacts.event("hold_closed", None, None)
    }
}
