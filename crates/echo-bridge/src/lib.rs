//! Bridge-owned orchestration. Real provider compatibility remains the M2 gate.
pub mod bridge_audio;
pub mod bridge_health;
pub mod bridge_input;
pub mod bridge_operator;
pub mod bridge_runtime;
pub mod bridge_trace;

/// Fixed diagnostics never contain input, credentials, audio or provider bodies.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("invalid input")]
    Input,
    #[error("authorization")]
    Authorization,
    #[error("audio")]
    Audio,
    #[error("wyoming")]
    Wyoming,
    #[error("unavailable")]
    Unavailable,
    #[error("deadline")]
    Deadline,
    #[error("decode")]
    Decode,
    #[error("trace")]
    Trace,
    #[error("health")]
    Health,
    #[error("screen")]
    Screen,
    #[error("control")]
    Control,
}

impl From<echo_audio::AudioError> for Error {
    fn from(_: echo_audio::AudioError) -> Self {
        Self::Audio
    }
}
impl From<echo_link::Error> for Error {
    fn from(error: echo_link::Error) -> Self {
        match error {
            echo_link::Error::Timeout => Self::Deadline,
            _ => Self::Wyoming,
        }
    }
}
impl From<echo_live::call::CallError> for Error {
    fn from(error: echo_live::call::CallError) -> Self {
        use echo_live::call::CallError;
        match error {
            CallError::Authorization => Self::Authorization,
            CallError::Deadline => Self::Deadline,
            CallError::Decode => Self::Decode,
            _ => Self::Unavailable,
        }
    }
}

#[cfg(test)]
mod bridge_spine;
