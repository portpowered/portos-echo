//! Shared screen messages. Transport, rendering and action policy belong to consumers.
//! Unknown object fields are ignored; unknown state/action tags are rejected.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[path = "screen_mouth.rs"]
pub mod mouth;

#[path = "screen_settings.rs"]
pub mod settings_v1;

#[path = "screen_settings_v2.rs"]
pub mod settings_v2;

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
    /// Sign-in is available or preparing a challenge.
    OpenAiRelink {
        message: String,
        starting: bool,
        muted: bool,
    },
    /// The current device-code challenge.
    OpenAiSignin {
        attempt_id: String,
        user_code: String,
        verification_uri: String,
        expires_at_unix: u64,
        muted: bool,
    },
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
    CancelOpenAiSignin { attempt_id: String },
    RenewOpenAiSignin { attempt_id: String },
}

#[cfg(test)]
mod tests {
    use super::{ScreenFrame, UiAction, UiState};
    use serde::de::DeserializeOwned;

    fn rejects<T: DeserializeOwned>(wire: &str, fragments: &[&str]) {
        let error = serde_json::from_str::<T>(wire)
            .err()
            .unwrap_or_else(|| unreachable!("invalid input decoded successfully"));
        assert_eq!(error.classify(), serde_json::error::Category::Data);
        let diagnostic = error.to_string();
        for fragment in fragments {
            assert!(diagnostic.contains(fragment), "{diagnostic:?} missing {fragment:?}");
        }
    }

    #[test]
    fn missing_and_unknown_discriminators() {
        for wire in ["{}", r#"{"action":"dismiss"}"#] {
            rejects::<UiState>(wire, &["missing field", "state"]);
        }
        for wire in ["{}", r#"{"state":"muted"}"#] {
            rejects::<UiAction>(wire, &["missing field", "action"]);
        }
        rejects::<UiState>(r#"{"state":"future"}"#, &["unknown variant", "future"]);
        rejects::<UiAction>(r#"{"action":"future"}"#, &["unknown variant", "future"]);
    }

    #[test]
    fn required_payloads_are_not_defaulted() {
        let cases = [
            (r#"{"state":"ready"}"#, "message"),
            (r#"{"state":"link_code"}"#, "user_code"),
            (r#"{"state":"link_code","user_code":"x"}"#, "verification_uri"),
            (
                r#"{"state":"link_code","user_code":"x","verification_uri":"x"}"#,
                "expires_at_unix",
            ),
            (r#"{"state":"approval"}"#, "id"),
            (r#"{"state":"approval","id":"x"}"#, "prompt"),
            (r#"{"state":"approval","id":"x","prompt":"x"}"#, "accept_label"),
            (
                r#"{"state":"approval","id":"x","prompt":"x","accept_label":"x"}"#,
                "deny_label",
            ),
            (r#"{"state":"emotion"}"#, "name"),
            (r#"{"state":"emotion","name":"x"}"#, "frame_ms"),
            (r#"{"state":"emotion","name":"x","frame_ms":0}"#, "frames"),
            (r#"{"state":"error","message":"x"}"#, "relink"),
        ];
        for (wire, field) in cases {
            rejects::<UiState>(wire, &["missing field", field]);
        }
        for tag in ["approve", "deny"] {
            rejects::<UiAction>(&format!(r#"{{"action":"{tag}"}}"#), &["missing field", "id"]);
        }
        rejects::<UiAction>(r#"{"action":"tap"}"#, &["missing field", "target"]);
        rejects::<ScreenFrame>(r#"{"state":{"state":"muted"}}"#, &["missing field", "seq"]);
        rejects::<ScreenFrame>(r#"{"seq":0}"#, &["missing field", "state"]);
    }

    #[test]
    fn invalid_payload_types() {
        for value in ["null", "42", "true", "[]", "{}"] {
            rejects::<UiState>(
                &format!(r#"{{"state":"ready","message":{value}}}"#),
                &["invalid type", "string"],
            );
            rejects::<UiAction>(
                &format!(r#"{{"action":"tap","target":{value}}}"#),
                &["invalid type", "string"],
            );
        }
        rejects::<UiState>(
            r#"{"state":"error","message":"x","relink":"true"}"#,
            &["invalid type", "boolean"],
        );
        rejects::<UiState>(
            r#"{"state":"emotion","name":"x","frame_ms":0,"frames":null}"#,
            &["invalid type", "sequence"],
        );
        rejects::<UiState>(
            r#"{"state":"emotion","name":"x","frame_ms":0,"frames":[1]}"#,
            &["invalid type", "string"],
        );
        rejects::<ScreenFrame>(r#"{"seq":0,"state":null}"#, &["invalid type"]);
        rejects::<UiState>(r#"{"state":null}"#, &["invalid type"]);
        rejects::<UiAction>(r#"{"action":42}"#, &["invalid type"]);
    }

    #[test]
    fn unsigned_fields_reject_invalid_numbers() {
        for value in ["-1", "0.5", r#""1""#, "18446744073709551616", "null"] {
            rejects::<ScreenFrame>(
                &format!(r#"{{"seq":{value},"state":{{"state":"muted"}}}}"#),
                &["invalid", "u64"],
            );
            rejects::<UiState>(
                &format!(r#"{{"state":"link_code","user_code":"x","verification_uri":"x","expires_at_unix":{value}}}"#),
                &["invalid", "u64"],
            );
            rejects::<UiState>(
                &format!(r#"{{"state":"emotion","name":"x","frame_ms":{value},"frames":[]}}"#),
                &["invalid", "u64"],
            );
        }
    }
}
