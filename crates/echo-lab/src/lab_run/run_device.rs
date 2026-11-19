use std::path::{Path, PathBuf};

use super::run_command::{argv, cancelled, controlled};
use super::run_lease::{LeaseClock, PlaybackGate};
use super::run_options::Options;
use super::run_report::Artifacts;
use super::{Driver, Step};

pub struct Device<'a> {
    options: &'a Options,
    hold: Option<(String, String)>,
    clock: LeaseClock,
    playback_end: std::cell::RefCell<PlaybackGate>,
    release_failed: bool,
    reply: Option<PathBuf>,
    pending: Option<super::run_command::Pending>,
    capture_window: bool,
    deferred_reply: bool,
}

impl<'a> Device<'a> {
    pub fn new(options: &'a Options) -> Self {
        Self {
            options,
            hold: None,
            clock: LeaseClock::new(),
            playback_end: std::cell::RefCell::new(PlaybackGate::default()),
            release_failed: false,
            reply: None,
            pending: None,
            capture_window: false,
            deferred_reply: false,
        }
    }

    pub(super) fn firmware(&self) -> Result<(), String> {
        crate::lab_gate::firmware(|name| self.property(name)).map(|_| ())
    }

    fn property(&self, name: &str) -> Result<String, String> {
        let output = controlled(
            &self.options.adb,
            &["shell".into(), "getprop".into(), name.into()],
            self.clock.deadline(false),
            self.options.cancel.as_deref(),
        )
        .map_err(|error| format!("firmware: {error}"))?;
        Ok(std::str::from_utf8(&output)
            .map_err(|_| "firmware: malformed property")?
            .trim()
            .to_owned())
    }

    fn host(&self, operation: &str, args: &[String]) -> Result<serde_json::Value, String> {
        let mut values = vec!["-c".into(), super::run_command::HOST.into(), operation.into()];
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
            Some(0..=8) if self.options.allow_quiet_hours => Ok(()),
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
        Ok(())
    }

    fn port(&self, operation: &str, values: &[(&str, &str)], cleanup: bool) -> Result<serde_json::Value, String> {
        let output = self.start_port(operation, values, cleanup)?.finish()?;
        let playing = matches!(operation, "file" | "text" | "file_only" | "text_only");
        let value: serde_json::Value = serde_json::from_slice(&output).map_err(|_| "port: malformed JSON")?;
        if playing {
            if value["held"] != true {
                return Err("play: missing held playback completion".into());
            }
            let end = value["playEndedAt"]
                .as_str()
                .ok_or("play: missing playback end timestamp")?;
            self.playback_end.borrow_mut().pending = Some(end.to_owned());
        }
        Ok(value)
    }

    fn start_port(
        &self,
        operation: &str,
        values: &[(&str, &str)],
        cleanup: bool,
    ) -> Result<super::run_command::Pending, String> {
        let pending = self.playback_end.borrow().pending.clone();
        if let Some(end) = pending.as_ref() {
            if end.is_empty() {
                return Err("playback completion unknown; coordinator recovery required".into());
            }
            let args = vec![
                "-c".into(),
                super::run_command::HOST.into(),
                "wait_until".into(),
                end.clone(),
            ];
            let output = controlled(
                Path::new(&self.options.python),
                &args,
                self.clock.deadline(cleanup),
                if cleanup { None } else { self.options.cancel.as_deref() },
            )
            .map_err(|error| format!("playback wait: {error}"))?;
            let value: serde_json::Value =
                serde_json::from_slice(&output).map_err(|_| "playback wait: malformed JSON")?;
            self.playback_end.borrow_mut().acknowledge(value["reached"] == true)?;
        }
        self.playback_end.borrow_mut().pending = None;
        let playing = matches!(operation, "file" | "text" | "file_only" | "text_only");
        let mut args = argv(operation, values)?;
        if playing && self.options.allow_quiet_hours {
            args.push("--allow-quiet-hours".into());
        }
        let child = super::run_command::Pending::start(
            &self.options.port,
            &args,
            self.clock.deadline(cleanup),
            if cleanup { None } else { self.options.cancel.as_deref() },
        )?;
        if playing {
            // Launch can queue remote audio even if waiting for the CLI later fails.
            self.playback_end.borrow_mut().pending = Some(String::new());
        }
        Ok(child)
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
        let lease = lease.clone();
        let output = path.to_str().ok_or("play: output path is not UTF-8")?;
        let mut values = vec![
            ("{ring}", self.options.ring.as_str()),
            ("{lease}", lease.as_str()),
            ("{output}", output),
        ];
        values.extend_from_slice(extra);
        if self.capture_window {
            return self.play_then_record(operation, &values, path);
        }
        let result = self
            .port(operation, &values, false)
            .map_err(|error| self.play_error(&error))?;
        if result["held"] != true || !path.is_file() {
            return Err(self.play_error("missing held recording"));
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
        self.reply = Some(path.to_path_buf());
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
    // Persistent sessions cannot wait for a ten-second acoustic capture after
    // every input. The configured owner waits on independent wire Played instead.
    pub(super) fn session_input(&mut self, step: &Step, index: usize, artifacts: &mut Artifacts) -> Result<(), String> {
        self.capture_window = true;
        let result = self.attempt(step, index, artifacts);
        self.deferred_reply = false;
        self.reply = None;
        result
    }

    fn play_then_record(&mut self, operation: &str, values: &[(&str, &str)], path: &Path) -> Result<(), String> {
        let result = self
            .port(&format!("{operation}_only"), values, false)
            .map_err(|e| self.play_error(&e))?;
        if result["held"] != true {
            return Err("play: missing held playback completion".into());
        }
        self.reply = Some(path.to_path_buf());
        self.deferred_reply = true;
        Ok(())
    }

    fn start_reply(&mut self) -> Result<(), String> {
        if !self.deferred_reply {
            return Ok(());
        }
        self.deferred_reply = false;
        self.audio_allowed()?;
        let (_, lease) = self.hold.as_ref().ok_or("record: no held session")?;
        let path = self.reply.as_ref().ok_or("record: missing reply path")?;
        self.pending = Some(
            self.start_port(
                "record",
                &[
                    ("{ring}", &self.options.ring),
                    ("{lease}", lease),
                    ("{output}", path.to_str().ok_or("record: invalid path")?),
                ],
                false,
            )
            .map_err(|error| format!("record: {error}"))?,
        );
        Ok(())
    }

    fn finish_reply(&mut self) -> Result<(), String> {
        if let Some(child) = self.pending.take() {
            let output = child
                .finish()
                .map_err(|_| "record: held reply capture failed; child reaped")?;
            let value: serde_json::Value =
                serde_json::from_slice(&output).map_err(|_| "record: malformed capture result")?;
            if value["success"] != true || value["hasAudio"] != true {
                return Err("record: held capture has no audio".into());
            }
            let path = self.reply.as_ref().ok_or("record: missing reply path")?;
            let result = self
                .host("wav", &[path.to_string_lossy().into_owned()])
                .map_err(|_| "record: invalid reply WAV")?;
            if result["reply_frames"].as_u64().is_none_or(|n| n == 0) {
                return Err("record: empty reply WAV".into());
            }
        }
        Ok(())
    }

    fn speech(&mut self, phrases: &[String]) -> Result<(), String> {
        self.finish_reply()?;
        let path = self.reply.as_ref().ok_or("speech expectation: no recorded reply")?;
        let transcript = controlled(
            &self.options.stt,
            &[path.to_string_lossy().into_owned()],
            self.clock.deadline(false),
            self.options.cancel.as_deref(),
        )
        .map_err(|_| "speech expectation: STT command failed")?;
        super::run_oracles::speech(&transcript, phrases)
    }

    fn screen(&self, golden: &Path, index: usize, artifacts: &mut Artifacts) -> Result<(), String> {
        let bytes = controlled(
            &self.options.adb,
            &["exec-out".into(), "screencap".into(), "-p".into()],
            self.clock.deadline(false),
            self.options.cancel.as_deref(),
        )
        .map_err(|_| "screen expectation: capture failed")?;
        let name = format!("screen-{index}.png");
        let path = artifacts.root.join(&name);
        std::fs::write(&path, bytes).map_err(|_| "screen expectation: cannot save screencap")?;
        artifacts.evidence.push(name);
        super::run_oracles::screen(&self.options.python, &path, golden)
    }

    fn frame(&mut self, name: &str, artifacts: &mut Artifacts) -> Result<(), String> {
        let (_, lease) = self.hold.as_ref().ok_or("frame capture: no held session")?;
        let path = super::run_oracles::frame_destination(&artifacts.root, name)?;
        let result = self
            .port(
                "snapshot",
                &[
                    ("{ring}", &self.options.ring),
                    ("{lease}", lease),
                    ("{output}", path.to_str().ok_or("frame capture: invalid path")?),
                ],
                false,
            )
            .map_err(|error| format!("frame capture: held snapshot failed: {error}"))?;
        if result["success"] != true || result["captured_at"].as_str().is_none() {
            return Err("frame capture: missing held capture confirmation".into());
        }
        let size = std::fs::metadata(&path)
            .map_err(|_| "frame capture: missing image")?
            .len();
        if size > 16 * 1024 * 1024 {
            return Err("frame capture: image exceeds size limit".into());
        }
        let bytes = std::fs::read(path).map_err(|_| "frame capture: missing image")?;
        super::run_jpeg::validate(&bytes).map_err(|reason| format!("frame capture: {reason}"))?;
        artifacts.evidence.push(name.to_owned());
        self.start_reply()
    }

    fn attempt(&mut self, step: &Step, index: usize, artifacts: &mut Artifacts) -> Result<(), String> {
        self.firmware()?;
        if matches!(step, Step::SayFile(_) | Step::SayText(_)) {
            self.finish_reply()?;
        }
        match step {
            Step::LinkCounts(expected) => artifacts.expect_link_counts(index, expected),
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
            Step::Restart { .. }
            | Step::Inject
            | Step::TraceAbsent(_)
            | Step::Unsupported
            | Step::Trace(_)
            | Step::Ui(_)
            | Step::Press(_)
            | Step::Budget(_, _)
            | Step::Mcp(_) => Err("step: capability is not implemented by device driver".into()),
            Step::Speech(phrases) => self.speech(phrases),
            Step::Screen(golden) => self.screen(golden, index, artifacts),
            Step::Frame(name) => self.frame(name, artifacts),
        }
    }
}

impl Driver for Device<'_> {
    fn execute(
        &mut self,
        step: &Step,
        index: usize,
        next: Option<&Step>,
        artifacts: &mut Artifacts,
    ) -> Result<(), String> {
        if let Step::Mcp(action) = step {
            return Err(format!("device: MCP {} control unavailable", action.name()));
        }
        self.capture_window = matches!(next, Some(Step::Frame(_)));
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
        let recording = self.finish_reply();
        let release = self.release(artifacts);
        match (recording, release) {
            (Err(a), Err(b)) => Err(format!("{a}; {b}")),
            (a, b) => a.and(b),
        }
    }
}

impl Device<'_> {
    fn release(&mut self, artifacts: &mut Artifacts) -> Result<(), String> {
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
