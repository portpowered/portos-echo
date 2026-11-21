//! The physical camera shutter is independent of the microphone mute.

/// A fresh switch reading or a sticky observation during a capture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Shutter {
    Open,
    Closed,
    Unknown,
}

#[cfg(target_os = "android")]
mod evdev;
#[cfg(target_os = "android")]
pub use evdev::Observation;

#[cfg(any(test, target_os = "android"))]
mod switch;
