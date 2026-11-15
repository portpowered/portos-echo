use std::path::Path;

use super::run_command::{argv, cancelled, controlled};
use super::run_lease::LeaseClock;
use super::run_options::Options;
use super::run_report::Artifacts;
use super::{Driver, Step};

pub struct Device<'a> {
    options: &'a Options,
    hold: Option<(String, String)>,
    clock: LeaseClock,
    aligned: bool,
    release_failed: bool,
}

impl<'a> Device<'a> {
    pub fn new(options: &'a Options) -> Self {
        Self {
            options,
            hold: None,
            clock: LeaseClock::new(),
            aligned: false,
            release_failed: false,
        }
    }

    fn firmware(&self) -> Result<(), String> {
        let args = ["shell", "getprop", "ro.build.version.incremental"].map(str::to_owned);
        let output = controlled(
            &self.options.adb,
            &args,
            self.clock.deadline(false),
            self.options.cancel.as_deref(),
        )
        .map_err(|error| format!("firmware: {error}"))?;
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

    fn host(&self, operation: &str, args: &[String]) -> Result<serde_json::Value, String> {
        let mut values = vec!["-c".into(), include_str!("run_host.py").into(), operation.into()];
        values.extend_from_slice(args);
        let data = controlled(
            Path::new(&self.options.python),
            &values,
            self.clock.deadline(false),
            self.options.cancel.as_deref(),
        )?;
        serde_json::from_slice(&data).map_err(|_| "host: malformed JSON".into())
    }

    fn audio_allowed(&mut self) -> Result<(), String> {
        let clock = self.host("clock", &[]).map_err(|error| format!("clock: {error}"))?;
        self.clock
            .observe(clock["monotonic_ms"].as_u64().ok_or("clock: missing monotonic time")?)?;
        match clock["hour"].as_u64() {
            Some(9..=23) => Ok(()),
            Some(0..=8) => Err("quiet hours: audio refused before 09:00 local time".into()),
            _ => Err("clock: invalid local hour".into()),
        }
    }

    fn open(&mut self, artifacts: &mut Artifacts) -> Result<(), String> {
        if self.hold.is_some() {
            return Ok(());
        }
        self.audio_allowed()?;
        self.clock.start();
        let result = self
            .port("hold", &[("{ring}", &self.options.ring)], false)
            .map_err(|error| format!("hold: {error}"))?;
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
        artifacts
            .sessions
            .push(format!("batch-hold-{}", artifacts.sessions.len() + 1));
        if !self.aligned {
            let path = artifacts.root.join("alignment.wav");
            self.play("chirp", &[], &path, artifacts)?;
            self.aligned = true;
        }
        Ok(())
    }

    fn port(&self, operation: &str, values: &[(&str, &str)], cleanup: bool) -> Result<serde_json::Value, String> {
        let output = controlled(
            &self.options.port,
            &argv(operation, values)?,
            self.clock.deadline(cleanup),
            if cleanup { None } else { self.options.cancel.as_deref() },
        )?;
        serde_json::from_slice(&output).map_err(|_| "port: malformed JSON".into())
    }

    fn rotate(&mut self, artifacts: &mut Artifacts) -> Result<(), String> {
        self.close(artifacts)?;
        artifacts.event("cooldown_started", None, None)?;
        let result = self.host("wait", &["25000".into()])?;
        if result["waited_ms"] != 25000 {
            return Err("cooldown: invalid wait acknowledgement".into());
        }
        artifacts.event("cooldown_completed", None, None)?;
        self.open(artifacts)
    }

    fn play(
        &mut self,
        operation: &str,
        extra: &[(&str, &str)],
        path: &Path,
        artifacts: &mut Artifacts,
    ) -> Result<(), String> {
        self.audio_allowed()?;
        if self.clock.renew() {
            self.rotate(artifacts)?;
            self.audio_allowed()?;
        }
        let (_, lease) = self.hold.as_ref().ok_or("play: no held session")?;
        let output = path.to_str().ok_or("play: output path is not UTF-8")?;
        let mut values = vec![
            ("{ring}", self.options.ring.as_str()),
            ("{lease}", lease),
            ("{output}", output),
        ];
        values.extend_from_slice(extra);
        let result = self
            .port(operation, &values, false)
            .map_err(|error| self.play_error(&error))?;
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
        let reply = self
            .host("reply", &trim)
            .map_err(|_| "play: invalid reply WAV or timing")?;
        if reply["reply_frames"].as_u64().is_none_or(|frames| frames == 0) {
            return Err("play: no post-playback reply audio".into());
        }
        Ok(())
    }

    fn play_error(&self, error: &str) -> String {
        if !cancelled(self.options.cancel.as_deref())
            && let Some((hold, _)) = &self.hold
            && let Ok(status) = self.port("status", &[("{hold}", hold)], false)
            && status["holds"].as_array().is_some_and(|holds| {
                holds
                    .iter()
                    .any(|h| h["id"] == *hold && h["state"] == "ended" && h["ending"]["reason"] == "preempted")
            })
        {
            return "play: preempted".into();
        }
        format!("play: {error}")
    }
}

impl Device<'_> {
    fn attempt(&mut self, step: &Step, index: usize, artifacts: &mut Artifacts) -> Result<(), String> {
        self.firmware()?;
        match step {
            Step::SayFile(path) => {
                self.open(artifacts)?;
                let file = path.to_str().ok_or("say: file path is not UTF-8")?;
                self.play(
                    "file",
                    &[("{file}", file)],
                    &artifacts.root.join(format!("reply-{index}.wav")),
                    artifacts,
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
                    artifacts,
                )
            }
            Step::Unsupported => Err("step: capability is not implemented by device driver".into()),
        }
    }
}

impl Driver for Device<'_> {
    fn execute(&mut self, step: &Step, index: usize, artifacts: &mut Artifacts) -> Result<(), String> {
        let result = self.attempt(step, index, artifacts);
        if result.as_ref().err().is_some_and(|error| error == "play: preempted") {
            artifacts.event("preempted", Some(index), None)?;
            self.close(artifacts)?;
            artifacts.event("retry_started", Some(index), None)?;
            return self.attempt(step, index, artifacts);
        }
        result
    }

    fn close(&mut self, artifacts: &mut Artifacts) -> Result<(), String> {
        let Some((hold, _)) = self.hold.take() else {
            self.clock.clear();
            if self.release_failed {
                return Err("cleanup: prior session release failed; coordinator recovery required".into());
            }
            return Ok(());
        };
        let result = self.port("release", &[("{hold}", &hold)], true).and_then(|value| {
            if value["holds"]
                .as_array()
                .is_some_and(|holds| holds.iter().any(|h| h["id"] == hold && h["state"] == "ended"))
            {
                Ok(())
            } else {
                Err("release did not confirm the held session ended".into())
            }
        });
        self.clock.clear();
        self.release_failed |= result.is_err();
        result.map_err(|error| format!("cleanup: port session release failed: {error}"))?;
        artifacts.event("hold_closed", None, None)
    }
}
