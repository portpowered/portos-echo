//! T2: real public types and `serde_json` with no transport.
use echo_screen_contract::{ScreenFrame, UiAction, UiState};
use serde::{Serialize, de::DeserializeOwned};
use std::{error::Error, fmt::Debug};

fn round_trip<T>(value: &T, wire: &str) -> Result<(), Box<dyn Error>>
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    assert_eq!(serde_json::to_string(value)?, wire);
    assert_eq!(&serde_json::from_str::<T>(wire)?, value);
    Ok(())
}

fn states() -> Vec<(UiState, &'static str)> {
    vec![
        (
            UiState::Ready {
                message: "Echo Live ready".into(),
            },
            r#"{"state":"ready","message":"Echo Live ready"}"#,
        ),
        (UiState::Connecting, r#"{"state":"connecting"}"#),
        (UiState::Connected, r#"{"state":"connected"}"#),
        (UiState::Listening, r#"{"state":"listening"}"#),
        (UiState::Thinking, r#"{"state":"thinking"}"#),
        (UiState::Speaking, r#"{"state":"speaking"}"#),
        (UiState::Muted, r#"{"state":"muted"}"#),
    ]
}

fn payload_states() -> Vec<(UiState, &'static str)> {
    vec![
        (
            UiState::LinkCode {
                user_code: "DEMO-CODE".into(),
                verification_uri: "https://example.invalid/verify".into(),
                expires_at_unix: 1_700_000_000,
            },
            r#"{"state":"link_code","user_code":"DEMO-CODE","verification_uri":"https://example.invalid/verify","expires_at_unix":1700000000}"#,
        ),
        (
            UiState::Approval {
                id: "approval-1".into(),
                prompt: "Continue?".into(),
                accept_label: "Allow".into(),
                deny_label: "Deny".into(),
            },
            r#"{"state":"approval","id":"approval-1","prompt":"Continue?","accept_label":"Allow","deny_label":"Deny"}"#,
        ),
        (
            UiState::Emotion {
                name: "happy".into(),
                frame_ms: 80,
                frames: vec!["frame-a".into(), "frame-b".into()],
            },
            r#"{"state":"emotion","name":"happy","frame_ms":80,"frames":["frame-a","frame-b"]}"#,
        ),
        (
            UiState::Error {
                message: "Please relink".into(),
                relink: true,
            },
            r#"{"state":"error","message":"Please relink","relink":true}"#,
        ),
        (
            UiState::Error {
                message: "Please relink".into(),
                relink: false,
            },
            r#"{"state":"error","message":"Please relink","relink":false}"#,
        ),
    ]
}

#[test]
fn all_states_and_nested_frames() -> Result<(), Box<dyn Error>> {
    for (state, wire) in states().into_iter().chain(payload_states()) {
        round_trip(&state, wire)?;
        let frame_wire = format!(r#"{{"seq":7,"state":{wire}}}"#);
        round_trip(&ScreenFrame { seq: 7, state }, &frame_wire)?;
    }
    Ok(())
}

#[test]
fn all_touch_actions() -> Result<(), Box<dyn Error>> {
    let fixtures = [
        (
            UiAction::Tap {
                target: "primary".into(),
            },
            r#"{"action":"tap","target":"primary"}"#,
        ),
        (
            UiAction::Approve {
                id: "approval-1".into(),
            },
            r#"{"action":"approve","id":"approval-1"}"#,
        ),
        (
            UiAction::Deny {
                id: "approval-1".into(),
            },
            r#"{"action":"deny","id":"approval-1"}"#,
        ),
        (UiAction::Relink, r#"{"action":"relink"}"#),
        (UiAction::Dismiss, r#"{"action":"dismiss"}"#),
    ];
    for (action, wire) in fixtures {
        round_trip(&action, wire)?;
    }
    Ok(())
}

fn preserved<T>(value: &T) -> Result<(), Box<dyn Error>>
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    assert_eq!(&serde_json::from_str::<T>(&serde_json::to_string(value)?)?, value);
    Ok(())
}

#[test]
fn text_and_integer_boundaries() -> Result<(), Box<dyn Error>> {
    for text in ["", "\u{e9}cho \u{1f308} \"quoted\"\nline"] {
        preserved(&UiState::Ready { message: text.into() })?;
        preserved(&UiState::Approval {
            id: text.into(),
            prompt: text.into(),
            accept_label: text.into(),
            deny_label: text.into(),
        })?;
        preserved(&UiState::Error {
            message: text.into(),
            relink: false,
        })?;
        preserved(&UiAction::Tap { target: text.into() })?;
        preserved(&UiAction::Approve { id: text.into() })?;
        preserved(&UiAction::Deny { id: text.into() })?;
        for n in [0, u64::MAX] {
            preserved(&UiState::LinkCode {
                user_code: text.into(),
                verification_uri: text.into(),
                expires_at_unix: n,
            })?;
            for frames in [vec![], vec![text.into(), "last".into()]] {
                preserved(&UiState::Emotion {
                    name: text.into(),
                    frame_ms: n,
                    frames,
                })?;
            }
            preserved(&ScreenFrame {
                seq: n,
                state: UiState::Muted,
            })?;
        }
    }
    Ok(())
}

#[test]
fn extra_metadata_is_ignored() -> Result<(), Box<dyn Error>> {
    let state: UiState = serde_json::from_str(r#"{"state":"ready","message":"hello","metadata":42}"#)?;
    round_trip(&state, r#"{"state":"ready","message":"hello"}"#)
}
