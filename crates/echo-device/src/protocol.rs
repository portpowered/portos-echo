//! Wyoming satellite events and their mapping to the native core.

use echo_device_sys::{Action, Core, Event, Protocol, VERSION};
use std::{
    ffi::CStr,
    io::{self, Write},
    sync::atomic::{AtomicI64, AtomicU32, Ordering},
    time::Duration,
};

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;

pub(super) static WYOMING: Wyoming = Wyoming { behind: Behind::new() };

pub(super) struct Wyoming {
    behind: Behind,
}

impl Protocol for Wyoming {
    fn connected(&self, core: &Core) {
        connected(core);
        log(b"client connected", b"");
    }

    fn event(&self, core: &Core, event: Event<'_>) {
        handle(core, &event);
    }

    fn disconnected(&self, core: &Core) {
        core.apply(Action::Link(false, false));
        log(b"client disconnected", b"");
    }

    fn start(&self, core: &Core) {
        start(core);
    }

    fn audio(&self, core: &Core, pcm: &[u8], timestamp: i64) {
        audio(core, pcm, timestamp, &self.behind);
    }

    fn played(&self, core: &Core) {
        core.send(c"played", None, &[]);
    }

    fn volume_changed(&self, core: &Core, volume: i32) {
        speaker(core, volume);
    }

    fn mute_changed(&self, core: &Core, muted: bool) {
        mute(core, muted);
    }

    fn print_mdns(&self, name: &CStr, port: i32) {
        let mut out = io::stdout().lock();
        let _ = mdns(&mut out, name, port).and_then(|()| out.flush());
    }
}

trait Satellite {
    fn apply(&self, action: Action<'_>);
    fn send(&self, kind: &CStr, data: Option<&CStr>, payload: &[u8]);
    fn offer(&self, kind: &CStr, data: Option<&CStr>, payload: &[u8], wait: Duration) -> bool;
    fn hang_up(&self);
    fn duplex(&self) -> bool;
    fn local_wake(&self) -> bool;
    fn name(&self) -> &CStr;
    fn model(&self) -> &CStr;
    fn wake_word(&self) -> Vec<u8>;
    fn listening(&self) -> bool;
    fn continue_reply(&self) -> bool;
    fn muted(&self) -> bool;
    fn volume(&self) -> i32;
    fn output_muted(&self) -> bool;
}

impl Satellite for Core {
    fn apply(&self, action: Action<'_>) {
        self.apply(action);
    }
    fn send(&self, kind: &CStr, data: Option<&CStr>, payload: &[u8]) {
        self.send(kind, data, payload);
    }
    fn offer(&self, kind: &CStr, data: Option<&CStr>, payload: &[u8], wait: Duration) -> bool {
        self.offer(kind, data, payload, wait)
    }
    fn hang_up(&self) {
        self.hang_up();
    }
    fn duplex(&self) -> bool {
        self.duplex()
    }
    fn local_wake(&self) -> bool {
        self.local_wake()
    }
    fn name(&self) -> &CStr {
        self.name()
    }
    fn model(&self) -> &CStr {
        self.model()
    }
    fn wake_word(&self) -> Vec<u8> {
        self.wake_word()
    }
    fn listening(&self) -> bool {
        self.listening()
    }
    fn continue_reply(&self) -> bool {
        self.continue_reply()
    }
    fn muted(&self) -> bool {
        self.muted()
    }
    fn volume(&self) -> i32 {
        self.volume()
    }
    fn output_muted(&self) -> bool {
        self.output_muted()
    }
}

fn connected(core: &impl Satellite) {
    core.apply(Action::Connected);
    core.apply(Action::Failsafe(30));
    core.apply(Action::Link(true, false));
    if core.muted() {
        mute(core, true);
    }
    speaker(core, core.volume());
}

fn handle(core: &impl Satellite, event: &Event<'_>) {
    match event.kind.to_bytes() {
        b"ping" => core.send(c"pong", None, &[]),
        b"describe" => info(core),
        b"run-satellite" => run_satellite(core, event),
        b"pause-satellite" => core.apply(Action::Pause),
        b"detection" => core.apply(Action::Detection),
        b"voice-stopped" => words(core, None),
        b"transcript" => words(core, Some(event)),
        b"audio-start" => reply_start(core, event),
        b"audio-chunk" => {
            core.apply(Action::Progress);
            core.apply(Action::AudioData(event.payload));
        }
        b"audio-stop" => {
            core.apply(Action::Progress);
            core.apply(Action::AudioEnd);
        }
        b"hassmic-continue" => {
            if !core.continue_reply() {
                log(b"hassmic-continue: no reply of a -D run to go on from, ignored", b"");
            }
        }
        b"hassmic-set-volume" => set_volume(core, event),
        b"hassmic-set-output-mute" => set_mute(core, event),
        b"hassmic-flush" => core.apply(Action::Flush),
        b"hassmic-end" => core.apply(Action::End),
        b"error" => server_error(core, event),
        _ => {}
    }
}

fn run_satellite(core: &impl Satellite, event: &Event<'_>) {
    if core.duplex() {
        if let Some(seconds) = event.failsafe_seconds() {
            core.apply(Action::Failsafe(seconds));
        } else {
            log(b"invalid hassmic_failsafe_seconds, ignored", b"");
        }
    }
    log(b"satellite: running", b"");
    core.apply(Action::Link(true, true));
}

fn words(core: &impl Satellite, transcript: Option<&Event<'_>>) {
    core.apply(Action::Progress);
    if let Some(event) = transcript {
        let mut text = [0; 256];
        let text = event.text(c"text", true, &mut text);
        log(b"transcript: ", text.to_bytes());
        core.apply(Action::Heard(text));
        if !core.duplex() {
            core.apply(Action::MicOff);
        }
    }
    if core.listening() {
        core.apply(Action::Thinking);
    }
}

fn reply_start(core: &impl Satellite, event: &Event<'_>) {
    let rate = event.audio_parameter(c"rate", 22050);
    let channels = event.audio_parameter(c"channels", 1);
    if let Some(width) = event.integer(c"width").filter(|width| *width != 2) {
        log(
            b"play: unsupported sample width ",
            format!("{width}, expect noise").as_bytes(),
        );
    }
    core.apply(Action::AudioBegin(rate, channels));
}

fn set_volume(core: &impl Satellite, event: &Event<'_>) {
    if let Some(volume) = event.speaker_volume() {
        // The core calls volume_changed synchronously, including on a no-op set.
        core.apply(Action::SetVolume(volume));
    } else {
        log(b"hassmic-set-volume: no volume 0..100, ignored", b"");
    }
}

fn set_mute(core: &impl Satellite, event: &Event<'_>) {
    if let Some(muted) = event.output_mute() {
        core.apply(Action::SetOutputMute(muted));
        speaker(core, core.volume());
    } else {
        log(b"hassmic-set-output-mute: no mute true or false, ignored", b"");
    }
}

fn server_error(core: &impl Satellite, event: &Event<'_>) {
    log(b"server error: ", event.json.to_bytes());
    let mut buffer = [0; 64];
    let error_id = event.text(c"code", false, &mut buffer);
    core.apply(if error_id == c"stt-no-text-recognized" {
        Action::NoSpeech
    } else {
        Action::Error
    });
}

fn start(core: &impl Satellite) {
    let mut data = Data::<256>::new();
    let stage = if core.local_wake() { "asr" } else { "wake" };
    let _ = write!(
        data,
        r#"{{"start_stage":"{stage}","end_stage":"tts","restart_on_end":false"#
    );
    if core.local_wake() {
        let _ = data.write_all(br#","wake_word_name":"#);
        wake_word(core, &mut data);
    }
    let _ = data.write_all(b"}");
    core.send(c"run-pipeline", Some(data.as_c_str()), &[]);
}

fn audio(core: &impl Satellite, pcm: &[u8], timestamp: i64, behind: &Behind) {
    let mut data = Data::<96>::new();
    let _ = write!(
        data,
        r#"{{"rate":16000,"width":2,"channels":1,"timestamp":{timestamp}}}"#
    );
    let sent = core.offer(c"audio-chunk", Some(data.as_c_str()), pcm, behind.wait());
    if behind.note(sent, timestamp) {
        core.hang_up();
    }
}

/// Microphone audio the client was too far behind to take: late audio is of
/// no use to a live talker, and waiting for the client would hold the core.
/// A burst, such as the replay of the wake word, gets a short wait to drain;
/// a client that stops reading gets none, and after `GIVE_UP_MS` the link is
/// dropped as a timed-out send drops it. Only the capture thread notes, under
/// the core lock.
struct Behind {
    since: AtomicI64,
    dropped: AtomicU32,
}

impl Behind {
    const fn new() -> Self {
        Self {
            since: AtomicI64::new(0),
            dropped: AtomicU32::new(0),
        }
    }

    const GRACE: Duration = Duration::from_millis(250);
    /// As long as the native send timeout lets a blocked send wait.
    const GIVE_UP_MS: i64 = 5000;

    fn wait(&self) -> Duration {
        if self.dropped.load(Ordering::Relaxed) == 0 {
            Self::GRACE
        } else {
            Duration::ZERO
        }
    }

    /// Returns whether to give up on the client.
    fn note(&self, sent: bool, timestamp: i64) -> bool {
        let dropped = self.dropped.load(Ordering::Relaxed);
        if !sent {
            if dropped == 0 {
                self.since.store(timestamp, Ordering::Relaxed);
                log(
                    b"mic: the client is not reading, mic audio dropped until it catches up",
                    b"",
                );
            } else if timestamp - self.since.load(Ordering::Relaxed) >= Self::GIVE_UP_MS {
                self.dropped.store(0, Ordering::Relaxed);
                log(b"mic: the client read nothing for 5 s, link dropped", b"");
                return true;
            }
            self.dropped.store(dropped.saturating_add(1), Ordering::Relaxed);
        } else if dropped > 0 {
            self.dropped.store(0, Ordering::Relaxed);
            let span = timestamp - self.since.load(Ordering::Relaxed);
            log(
                b"mic: the client caught up, dropped ",
                format!("{dropped} chunks over {span} ms").as_bytes(),
            );
        }
        false
    }
}

fn mute(core: &impl Satellite, muted: bool) {
    let data = if muted {
        c"{\"muted\":true}"
    } else {
        c"{\"muted\":false}"
    };
    core.send(c"hassmic-mute", Some(data), &[]);
}

fn speaker(core: &impl Satellite, volume: i32) {
    let mut data = Data::<48>::new();
    let muted = core.output_muted();
    let _ = write!(data, r#"{{"volume":{volume},"muted":{muted}}}"#);
    core.send(c"hassmic-speaker", Some(data.as_c_str()), &[]);
}

fn info(core: &impl Satellite) {
    let mut data = Data::<1024>::new();
    let _ = data
        .write_all(br#"{"asr":[],"tts":[],"handle":[],"intent":[],"wake":[],"mic":[],"snd":[],"satellite":{"name":""#);
    let _ = data.write_all(core.name().to_bytes());
    let _ = data.write_all(br#"","attribution":{"name":"hassmic","url":""},"installed":true,"description":""#);
    let _ = data.write_all(core.model().to_bytes());
    let _ = write!(
        data,
        r#" with Amazon audio front end","version":"{VERSION}","area":null,"has_vad":false,"active_wake_words":["#
    );
    if core.local_wake() {
        wake_word(core, &mut data);
    }
    let _ = data.write_all(br#"],"max_active_wake_words":1,"supports_trigger":true}}"#);
    core.send(c"info", Some(data.as_c_str()), &[]);
}

fn wake_word<const N: usize>(core: &impl Satellite, data: &mut Data<N>) {
    let mut word = core.wake_word();
    word.truncate(63);
    for byte in &mut word {
        if !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')) {
            *byte = b'_';
        }
    }
    let _ = data.write_all(b"\"");
    let _ = data.write_all(&word);
    let _ = data.write_all(b"\"");
}

fn mdns(out: &mut impl Write, name: &CStr, port: i32) -> io::Result<()> {
    out.write_all(b"<?xml version=\"1.0\" standalone='no'?>\n<!DOCTYPE service-group SYSTEM \"avahi-service.dtd\">\n<service-group>\n  <name replace-wildcards=\"yes\">")?;
    out.write_all(name.to_bytes())?;
    writeln!(
        out,
        " %h</name>\n  <service><type>_wyoming._tcp</type><port>{port}</port></service>\n</service-group>"
    )
}

fn log(prefix: &[u8], text: &[u8]) {
    let mut stderr = io::stderr().lock();
    let _ = stderr
        .write_all(prefix)
        .and_then(|()| stderr.write_all(text))
        .and_then(|()| stderr.write_all(b"\n"));
}

// Retain the pinned provider's snprintf limits and NUL termination, including
// truncation of a long raw argv name. The capture path formats on the stack.
struct Data<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> Data<N> {
    fn new() -> Self {
        Self { bytes: [0; N], len: 0 }
    }

    fn as_c_str(&self) -> &CStr {
        CStr::from_bytes_with_nul(&self.bytes[..=self.len]).unwrap_or(c"")
    }
}

impl<const N: usize> Write for Data<N> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = bytes.len().min(N - 1 - self.len);
        self.bytes[self.len..self.len + count].copy_from_slice(&bytes[..count]);
        self.len += count;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
