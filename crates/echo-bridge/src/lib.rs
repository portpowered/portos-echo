//! Bridge-owned orchestration. Real provider compatibility remains the M2 gate.
pub mod bridge_audio;
#[path = "bridge_codex/bridge_codex.rs"]
pub mod bridge_codex;
pub mod bridge_health;
pub mod bridge_input;
#[path = "bridge_mcp/bridge_mcp.rs"]
pub mod bridge_mcp;
#[path = "bridge_muse/bridge_muse.rs"]
pub mod bridge_muse;
#[path = "bridge_notes/notes.rs"]
mod bridge_notes;
pub mod bridge_operator;
mod bridge_private;
pub mod bridge_runtime;
#[path = "bridge_settings/bridge_settings.rs"]
pub mod bridge_settings;
#[path = "bridge_time/bridge_time.rs"]
pub mod bridge_time;
pub mod bridge_trace;
#[path = "bridge_voice/bridge_voice.rs"]
pub mod bridge_voice;

/// Fixed diagnostics never contain input, credentials, audio or provider bodies.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("invalid input")]
    Input,
    #[error("{0}")]
    Prompt(&'static str),
    #[error("{0}")]
    PromptConfig(String),
    #[error("codex.home: required for Codex runtime")]
    CodexHomeRequired,
    #[error("codex.home: private ownership unavailable")]
    CodexHome,
    #[error("codex.binary: unavailable")]
    CodexBinary,
    #[error("codex.ca_file: unavailable")]
    CodexCaFile,
    #[error("use signin with the configured Codex home")]
    CodexImport,
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
    /// The model finished no reply inside the response window. Ends that run only.
    #[error("no response")]
    NoResponse,
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
    #[error("{0}")]
    Mcp(echo_mcp_auth::AuthError),
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
