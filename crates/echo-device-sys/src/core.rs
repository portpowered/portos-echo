//! Lock and lifetime boundary for callbacks from the native satellite.

#![expect(unsafe_code, reason = "Native core calls and callback ABI")]

use crate::wire::{self, Event, Reader};
use std::{
    ffi::{CStr, c_char, c_int, c_uint, c_void},
    marker::PhantomData,
    slice,
    sync::{
        OnceLock,
        atomic::{AtomicI32, Ordering},
    },
};

static PROTOCOL: OnceLock<&'static dyn Protocol> = OnceLock::new();
// Only accessed under core_lock. The native non-threaded listener closes the
// borrowed descriptor after serve returns.
static CLIENT: AtomicI32 = AtomicI32::new(-1);

unsafe extern "C" {
    fn echo_hassmic_register(
        serve: extern "C" fn(c_int),
        start: extern "C" fn(),
        audio: extern "C" fn(*const c_void, usize),
        played: extern "C" fn(),
        volume_changed: extern "C" fn(c_int),
        mute_changed: extern "C" fn(c_int),
        print_mdns: extern "C" fn(),
    );
    fn echo_hassmic_lock();
    fn echo_hassmic_unlock();
    fn echo_hassmic_shutdown(fd: c_int);
    fn echo_hassmic_listening() -> c_int;
    fn echo_hassmic_detection();
    fn echo_hassmic_thinking();
    fn echo_hassmic_heard(text: *const c_char);
    fn echo_hassmic_model() -> *const c_char;
    fn echo_hassmic_wake_word() -> *const c_char;
    fn echo_hassmic_millis() -> i64;
    static core_name: *const c_char;
    static core_port: c_int;
    static core_duplex: c_int;
    static core_local_wake: c_int;
    fn core_muted() -> c_int;
    fn core_volume() -> c_int;
    fn core_output_mute(set: c_int) -> c_int;
    fn core_set_volume(volume: c_int);
    fn core_link(connected: c_int, ready: c_int);
    fn core_link_pause();
    fn core_mic_off();
    fn core_continue() -> c_int;
    fn core_tts_begin(rate: c_uint, channels: c_uint, media: c_int);
    fn core_tts_data(pcm: *const c_void, len: usize);
    fn core_tts_end();
    fn core_duplex_connected();
    fn core_duplex_failsafe(seconds: c_int);
    fn core_duplex_progress();
    fn core_duplex_flush();
    fn core_duplex_end();
    fn core_no_speech();
    fn core_error();
}

/// Rust implementation of the seven hooks exposed by the native table, plus
/// connection and event handling inside the non-threaded serve hook.
pub trait Protocol: Sync {
    fn connected(&self, core: &Core);
    fn event(&self, core: &Core, event: Event<'_>);
    fn disconnected(&self, core: &Core);
    fn start(&self, core: &Core);
    fn audio(&self, core: &Core, pcm: &[u8], timestamp: i64);
    fn played(&self, core: &Core);
    fn volume_changed(&self, core: &Core, volume: i32);
    fn mute_changed(&self, core: &Core, muted: bool);
    fn print_mdns(&self, name: &CStr, port: i32);
}

pub(super) fn register(protocol: &'static dyn Protocol) {
    let _ = PROTOCOL.set(protocol);
    // SAFETY: run admits one caller before the native entry or any worker.
    // All callbacks have the exact native ABI and process lifetime.
    unsafe { echo_hassmic_register(serve, start, audio, played, volume, mute, print_mdns) };
}

fn with_core(callback: impl FnOnce(&dyn Protocol, &Core)) {
    if let Some(protocol) = PROTOCOL.get() {
        callback(*protocol, &Core { thread: PhantomData });
    }
}

fn with_core_lock(callback: impl FnOnce(&dyn Protocol, &Core)) {
    struct Unlock;
    impl Drop for Unlock {
        fn drop(&mut self) {
            // SAFETY: This guard is created and dropped on the acquiring thread.
            unsafe { echo_hassmic_unlock() };
        }
    }
    // SAFETY: Only the native serve hook takes this path, with no lock held.
    unsafe { echo_hassmic_lock() };
    let _guard = Unlock;
    with_core(callback);
}

extern "C" fn serve(fd: c_int) {
    let Some(mut reader) = Reader::new(fd) else {
        return;
    };
    with_core_lock(|protocol, core| {
        CLIENT.store(fd, Ordering::Relaxed);
        protocol.connected(core);
    });
    while let Some(event) = reader.read() {
        with_core_lock(|protocol, core| protocol.event(core, event));
    }
    with_core_lock(|protocol, core| {
        CLIENT.store(-1, Ordering::Relaxed);
        protocol.disconnected(core);
    });
}

extern "C" fn start() {
    with_core(|protocol, core| protocol.start(core));
}

extern "C" fn audio(pcm: *const c_void, len: usize) {
    if len == 0 || len > wire::MAX_PAYLOAD || pcm.is_null() {
        return;
    }
    // SAFETY: The core gives the callback a live PCM buffer of len bytes.
    // Its borrow ends before control returns to C. The clock takes no lock.
    let (pcm, timestamp) = unsafe { (slice::from_raw_parts(pcm.cast(), len), echo_hassmic_millis()) };
    with_core(|protocol, core| protocol.audio(core, pcm, timestamp));
}

extern "C" fn played() {
    with_core(|protocol, core| protocol.played(core));
}

extern "C" fn volume(percent: c_int) {
    with_core(|protocol, core| protocol.volume_changed(core, percent));
}

extern "C" fn mute(muted: c_int) {
    with_core(|protocol, core| protocol.mute_changed(core, muted != 0));
}

extern "C" fn print_mdns() {
    if let Some(protocol) = PROTOCOL.get() {
        // SAFETY: The startup-only hook runs after name and port are assigned,
        // before workers. The name is a live NUL-terminated argv/static string.
        unsafe { protocol.print_mdns(CStr::from_ptr(core_name), core_port) };
    }
}

/// Commands understood by the core under its mutex.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action<'a> {
    Connected,
    Failsafe(i32),
    Link(bool, bool),
    Pause,
    Detection,
    Thinking,
    Progress,
    Heard(&'a CStr),
    MicOff,
    AudioBegin(u32, u32),
    AudioData(&'a [u8]),
    AudioEnd,
    SetVolume(i32),
    SetOutputMute(bool),
    Flush,
    End,
    NoSpeech,
    Error,
}

/// Access to the native core while its mutex is held on this thread.
pub struct Core {
    thread: PhantomData<*mut ()>,
}

impl Core {
    pub fn apply(&self, action: Action<'_>) {
        // SAFETY: The token is available only on a core_lock-held thread.
        // The PCM passed to core_tts_data is copied into the native queue,
        // and screen_words reads the terminated text during the call.
        unsafe {
            match action {
                Action::Connected => core_duplex_connected(),
                Action::Failsafe(seconds) => core_duplex_failsafe(seconds),
                Action::Link(connected, ready) => core_link(connected.into(), ready.into()),
                Action::Pause => core_link_pause(),
                Action::Detection => echo_hassmic_detection(),
                Action::Thinking => echo_hassmic_thinking(),
                Action::Progress => core_duplex_progress(),
                Action::Heard(text) => echo_hassmic_heard(text.as_ptr()),
                Action::MicOff => core_mic_off(),
                Action::AudioBegin(rate, channels) => core_tts_begin(rate, channels, 0),
                Action::AudioData(pcm) => core_tts_data(pcm.as_ptr().cast(), pcm.len()),
                Action::AudioEnd => core_tts_end(),
                Action::SetVolume(volume) => core_set_volume(volume),
                Action::SetOutputMute(muted) => {
                    core_output_mute(muted.into());
                }
                Action::Flush => core_duplex_flush(),
                Action::End => core_duplex_end(),
                Action::NoSpeech => core_no_speech(),
                Action::Error => core_error(),
            }
        }
    }

    pub fn send(&self, kind: &CStr, data: Option<&CStr>, payload: &[u8]) {
        let fd = CLIENT.load(Ordering::Relaxed);
        if fd >= 0 && kind.to_bytes().len() < 48 && !wire::write(fd, kind, data, payload) {
            // SAFETY: The core lock keeps the borrowed descriptor live. Shutdown
            // wakes the reader; only the C listener closes the descriptor.
            unsafe { echo_hassmic_shutdown(fd) };
            CLIENT.store(-1, Ordering::Relaxed);
        }
    }

    #[must_use]
    pub fn duplex(&self) -> bool {
        // SAFETY: The mode is fixed by argv before callback admission.
        unsafe { core_duplex != 0 }
    }

    #[must_use]
    pub fn local_wake(&self) -> bool {
        // SAFETY: The core lock protects the wake configuration.
        unsafe { core_local_wake != 0 }
    }

    #[must_use]
    pub fn name(&self) -> &CStr {
        // SAFETY: Startup assigns a terminated argv/static string that remains live.
        unsafe { CStr::from_ptr(core_name) }
    }

    #[must_use]
    pub fn model(&self) -> &CStr {
        // SAFETY: The board's model is a static, terminated C string.
        unsafe { CStr::from_ptr(echo_hassmic_model()) }
    }

    #[must_use]
    pub fn wake_word(&self) -> Vec<u8> {
        // SAFETY: The core lock keeps the selected terminated word valid
        // while it is copied. No pointer to mutable wake state escapes.
        unsafe { CStr::from_ptr(echo_hassmic_wake_word()).to_bytes().to_vec() }
    }

    #[must_use]
    pub fn listening(&self) -> bool {
        // SAFETY: The core lock protects the pipeline state.
        unsafe { echo_hassmic_listening() != 0 }
    }

    #[must_use]
    pub fn continue_reply(&self) -> bool {
        // SAFETY: The core lock protects the reply state.
        unsafe { core_continue() != 0 }
    }

    #[must_use]
    pub fn muted(&self) -> bool {
        // SAFETY: The core lock protects effective microphone mute.
        unsafe { core_muted() != 0 }
    }

    #[must_use]
    pub fn volume(&self) -> i32 {
        // SAFETY: The core lock protects speaker settings.
        unsafe { core_volume() }
    }

    #[must_use]
    pub fn output_muted(&self) -> bool {
        // SAFETY: The core lock protects speaker settings. -1 means read.
        unsafe { core_output_mute(-1) != 0 }
    }
}
