//! Shared screen messages. Transport, rendering and action policy belong to consumers.
//! Unknown object fields are ignored; unknown state/action tags are rejected.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A complete screen presentation, serialized with a `snake_case` `state` tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UiState {
    /// Initial message before a session starts.
    Ready { message: String },
    /// Establishing the bridge session.
    Connecting,
    /// Session established.
    Connected,
    /// Capturing the user's turn.
    Listening,
    /// Preparing a response.
    Thinking,
    /// Playing a response.
    Speaking,
    /// Microphone muted.
    Muted,
    /// Device authorization instructions; expiry is nonnegative Unix seconds.
    LinkCode {
        user_code: String,
        verification_uri: String,
        expires_at_unix: u64,
    },
    /// Confirmation prompt correlated by its opaque identifier.
    Approval {
        id: String,
        prompt: String,
        accept_label: String,
        deny_label: String,
    },
    /// Ordered animation frames; duration is milliseconds per frame.
    Emotion {
        name: String,
        frame_ms: u64,
        frames: Vec<String>,
    },
    /// Failure presentation, optionally offering relinking.
    Error { message: String, relink: bool },
}

/// An ordered presentation. Sequence lifecycle is owned by the screen server.
/// Consumers must preserve the full unsigned integer range, including in JS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ScreenFrame {
    pub seq: u64,
    pub state: UiState,
}

/// A touch intent. Consumers validate correlation and execute any action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum UiAction {
    Tap { target: String },
    Approve { id: String },
    Deny { id: String },
    Relink,
    Dismiss,
}

#[cfg(test)]
mod invalid;
