use super::*;
use std::cell::{Cell, RefCell};

#[derive(Debug, PartialEq, Eq)]
enum Observation {
    Core(String),
    Frame(Vec<u8>, Vec<u8>, Vec<u8>),
}

struct Fake {
    observations: RefCell<Vec<Observation>>,
    duplex: bool,
    local_wake: bool,
    muted: bool,
    output_muted: Cell<bool>,
}

impl Default for Fake {
    fn default() -> Self {
        Self {
            observations: RefCell::default(),
            duplex: true,
            local_wake: true,
            muted: false,
            output_muted: Cell::new(false),
        }
    }
}

impl Satellite for Fake {
    fn apply(&self, action: Action<'_>) {
        self.record(Observation::Core(format!("{action:?}")));
        if let Action::SetOutputMute(muted) = action {
            self.output_muted.set(muted);
        }
    }

    fn send(&self, kind: &CStr, data: Option<&CStr>, payload: &[u8]) {
        self.record(frame(kind.to_bytes(), data.unwrap_or(c"").to_bytes(), payload));
    }

    fn duplex(&self) -> bool {
        self.duplex
    }
    fn local_wake(&self) -> bool {
        self.local_wake
    }
    fn name(&self) -> &CStr {
        c"Lab-Sat"
    }
    fn model(&self) -> &CStr {
        c"Echo Show 8"
    }
    fn wake_word(&self) -> Vec<u8> {
        b"computer en-US".to_vec()
    }
    fn listening(&self) -> bool {
        true
    }
    fn continue_reply(&self) -> bool {
        self.record(Observation::Core("Continue".into()));
        false
    }
    fn muted(&self) -> bool {
        self.muted
    }
    fn volume(&self) -> i32 {
        40
    }
    fn output_muted(&self) -> bool {
        self.output_muted.get()
    }
}

impl Fake {
    fn record(&self, observation: Observation) {
        self.observations.borrow_mut().push(observation);
    }

    fn accept(&self, kind: &CStr, json: &CStr, payload: &[u8]) {
        handle(self, &Event { kind, json, payload });
    }

    fn assert_actions(&self, expected: &[Action<'_>]) {
        let expected: Vec<_> = expected
            .iter()
            .map(|action| Observation::Core(format!("{action:?}")))
            .collect();
        assert_eq!(self.observations.take(), expected);
    }
}

fn frame(kind: &[u8], data: &[u8], payload: &[u8]) -> Observation {
    Observation::Frame(kind.to_vec(), data.to_vec(), payload.to_vec())
}

#[test]
fn connection_and_callbacks_preserve_mute_order_and_speaker_values() {
    let core = Fake {
        muted: true,
        ..Fake::default()
    };
    connected(&core);
    mute(&core, false);
    core.accept(c"hassmic-set-output-mute", c"{\"data\":{\"mute\":true}}", &[]);
    speaker(&core, 77);
    assert_eq!(
        core.observations.take(),
        [
            Observation::Core("Connected".into()),
            Observation::Core("Failsafe(30)".into()),
            Observation::Core("Link(true, false)".into()),
            frame(b"hassmic-mute", br#"{"muted":true}"#, b""),
            frame(b"hassmic-speaker", br#"{"volume":40,"muted":false}"#, b""),
            frame(b"hassmic-mute", br#"{"muted":false}"#, b""),
            Observation::Core("SetOutputMute(true)".into()),
            frame(b"hassmic-speaker", br#"{"volume":40,"muted":true}"#, b""),
            frame(b"hassmic-speaker", br#"{"volume":77,"muted":true}"#, b""),
        ]
    );
}

#[test]
fn capture_info_and_pong_keep_the_fixed_wire_values() {
    let core = Fake::default();
    core.accept(c"describe", c"{}", &[]);
    start(&core);
    audio(&core, b"\x13\x00", 1234);
    core.accept(c"ping", c"{}", &[]);
    let info = format!(
        concat!(
            r#"{{"asr":[],"tts":[],"handle":[],"intent":[],"wake":[],"mic":[],"snd":[],"satellite":{{"name":"Lab-Sat","#,
            r#""attribution":{{"name":"hassmic","url":""}},"installed":true,"description":"Echo Show 8 with Amazon audio front end","#,
            r#""version":"{}","area":null,"has_vad":false,"active_wake_words":["computer_en-US"],"max_active_wake_words":1,"supports_trigger":true}}}}"#
        ),
        VERSION
    );
    assert_eq!(
        core.observations.take(),
        [
            frame(b"info", info.as_bytes(), b""),
            frame(
                b"run-pipeline",
                br#"{"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"computer_en-US"}"#,
                b""
            ),
            frame(
                b"audio-chunk",
                br#"{"rate":16000,"width":2,"channels":1,"timestamp":1234}"#,
                b"\x13\x00"
            ),
            frame(b"pong", b"", b""),
        ]
    );
    let remote = Fake {
        local_wake: false,
        ..Fake::default()
    };
    start(&remote);
    assert_eq!(
        remote.observations.take(),
        [frame(
            b"run-pipeline",
            br#"{"start_stage":"wake","end_stage":"tts","restart_on_end":false}"#,
            b""
        ),]
    );
}

#[test]
fn controls_validate_existing_placements_and_preserve_run_order() {
    let core = Fake::default();
    core.accept(c"run-satellite", c"{}", &[]);
    core.accept(c"run-satellite", c"{\"data\":{\"hassmic_failsafe_seconds\":2.0}}", &[]);
    core.accept(c"run-satellite", c"{}\n{\"hassmic_failsafe_seconds\":15}", &[]);
    core.accept(c"hassmic-set-volume", c"{}\n{\"volume\":100}", &[]);
    core.accept(c"hassmic-set-volume", c"{\"data\":{\"volume\":101}}", &[]);
    core.accept(c"hassmic-set-output-mute", c"{}\n{\"mute\":1}", &[]);
    core.accept(c"pause-satellite", c"{}", &[]);
    core.accept(c"detection", c"{}", &[]);
    core.accept(c"hassmic-flush", c"{}", &[]);
    core.accept(c"hassmic-end", c"{}", &[]);
    core.assert_actions(&[
        Action::Failsafe(30),
        Action::Link(true, true),
        Action::Link(true, true),
        Action::Failsafe(15),
        Action::Link(true, true),
        Action::SetVolume(100),
        Action::Pause,
        Action::Detection,
        Action::Flush,
        Action::End,
    ]);
}

#[test]
fn reply_progress_defaults_and_error_actions_match_the_core_contract() {
    let core = Fake {
        duplex: false,
        ..Fake::default()
    };
    core.accept(c"voice-stopped", c"{}", &[]);
    core.accept(c"transcript", c"{\"text\":\"inline\"}\n{\"text\":\"heard\"}", &[]);
    core.accept(c"synthesize", c"{}", &[]);
    core.accept(c"audio-start", c"{}", &[]);
    core.accept(c"audio-start", c"{}\n{\"rate\":16000,\"channels\":2,\"width\":1}", &[]);
    core.accept(c"audio-chunk", c"{}", b"\x13\x00");
    core.accept(c"audio-stop", c"{}", &[]);
    core.accept(c"error", c"{}\n{\"code\":\"stt-no-text-recognized\"}", &[]);
    core.accept(c"error", c"{\"data\":{\"code\":\"tts-failed\"}}", &[]);
    core.assert_actions(&[
        Action::Progress,
        Action::Thinking,
        Action::Progress,
        Action::Heard(c"heard"),
        Action::MicOff,
        Action::Thinking,
        Action::AudioBegin(22050, 1),
        Action::AudioBegin(16000, 2),
        Action::Progress,
        Action::AudioData(b"\x13\x00"),
        Action::Progress,
        Action::AudioEnd,
        Action::NoSpeech,
        Action::Error,
    ]);
    core.accept(c"hassmic-continue", c"{}", &[]);
    assert_eq!(core.observations.take(), [Observation::Core("Continue".into())]);
}
