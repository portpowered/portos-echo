//! T2: real public types and `serde_json` with no transport.
use echo_screen_contract::{ScreenFrame, UiAction, UiState};
use serde::{Serialize, de::DeserializeOwned};
use std::io::Write as _;
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
                verification_uri: "https://example.invalid/verify".into(), // lint-rules: allow-literal UiState wire serialization assertion fixture
                expires_at_unix: 1_700_000_000,
            },
            r#"{"state":"link_code","user_code":"DEMO-CODE","verification_uri":"https://example.invalid/verify","expires_at_unix":1700000000}"#, // lint-rules: allow-literal UiState wire serialization assertion fixture
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

#[test]
fn settings_v1_sparse_wire_and_decimal_cap_are_lossless() -> Result<(), Box<dyn Error>> {
    use echo_screen_contract::settings_v1::{PromptSaveV1, SaveResultV1, SettingsPatchV1};
    for wire in [
        r#"{"version":1,"settings":{"voice":{"session_listen_enabled":false}}}"#,
        r#"{"version":1,"settings":{"codex":{"voice":"default"}}}"#,
        r#"{"version":1,"settings":{"agent":{"daily_request_cap":"9223372036854775807","max_tool_calls":8}}}"#,
        r#"{"version":1,"settings":{"agent":{"daily_request_cap":"0"}}}"#,
    ] {
        let patch: SettingsPatchV1 = serde_json::from_str(wire)?;
        assert!(patch.settings.is_nonempty());
        round_trip(&patch, wire)?;
    }
    let wire = r#"{"version":1,"text":"\"<script>inert</script>\"\n日本語"}"#;
    round_trip(&serde_json::from_str::<PromptSaveV1>(wire)?, wire)?;
    let wire = r#"{"version":1,"restart_pending":false}"#;
    round_trip(&serde_json::from_str::<SaveResultV1>(wire)?, wire)
}

#[test]
fn settings_v1_rejects_versions_extras_nulls_and_lossy_caps() {
    use echo_screen_contract::settings_v1::{PromptSaveV1, SettingsPatchV1};
    for wire in [
        r#"{"settings":{"agent":{"max_tool_calls":8}}}"#,
        r#"{"version":2,"settings":{"agent":{"max_tool_calls":8}}}"#,
        r#"{"version":"1","settings":{"agent":{"max_tool_calls":8}}}"#,
        r#"{"version":1,"settings":{"agent":{"max_tool_calls":8}},"extra":0}"#,
        r#"{"version":1,"settings":{"home":{"dir":"forbidden"}}}"#,
        r#"{"version":1,"settings":{"voice":{"session_max_seconds":600}}}"#,
        r#"{"version":1,"settings":{"voice":null}}"#,
        r#"{"version":1,"settings":{"voice":{"session_listen_enabled":null}}}"#,
        r#"{"version":1,"settings":{"codex":{}}}"#,
        r#"{"version":1,"settings":{"codex":{"voice":"default","extra":0}}}"#,
        r#"{"version":1,"settings":{"agent":{"daily_request_cap":9007199254740993}}}"#,
        r#"{"version":1,"settings":{"agent":{"daily_request_cap":"01"}}}"#,
        r#"{"version":1,"settings":{"agent":{"daily_request_cap":"-1"}}}"#,
        r#"{"version":1,"settings":{"agent":{"daily_request_cap":""}}}"#,
        r#"{"version":1,"settings":{"agent":{"daily_request_cap":"18446744073709551615"}}}"#,
        r#"{"version":1,"settings":{"agent":{"max_tool_calls":1,"max_tool_calls":2}}}"#,
    ] {
        assert!(serde_json::from_str::<SettingsPatchV1>(wire).is_err(), "{wire}");
    }
    for wire in [
        r#"{"version":1,"settings":{}}"#,
        r#"{"version":1,"settings":{"voice":{}}}"#,
        r#"{"version":1,"settings":{"agent":{}}}"#,
        r#"{"version":1,"settings":{"codex":{"voice":"default"},"agent":{}}}"#,
    ] {
        assert!(
            !serde_json::from_str::<SettingsPatchV1>(wire)
                .unwrap()
                .settings
                .is_nonempty()
        );
    }
    for wire in [
        r#"{"version":1,"text":null}"#,
        r#"{"version":1,"text":"a","path":"other"}"#,
    ] {
        assert!(serde_json::from_str::<PromptSaveV1>(wire).is_err());
    }
}

#[test]
fn settings_v2_sparse_wire_preserves_decimal_cap_and_refuses_other_shapes() -> Result<(), Box<dyn Error>> {
    use echo_screen_contract::settings_v1::SettingsPatchV1;
    use echo_screen_contract::settings_v2::SettingsPatchV2;

    let wire = r#"{"version":2,"settings":{"codex":{"web_search":"cached","delegate_model":"synthetic-runner"},"agent":{"daily_request_cap":"9223372036854775807","max_tool_calls":6},"portos":{"scopes":["endpoint:read"],"allow_tools":["endpoint_query"]}}}"#;
    let patch = serde_json::from_str::<SettingsPatchV2>(wire)?;
    assert!(patch.settings.valid_shape());
    round_trip(&patch, wire)?;
    assert!(serde_json::from_str::<SettingsPatchV1>(wire).is_err());
    writeln!(std::io::stdout().lock(), "{wire}").unwrap();

    for invalid in [
        r#"{"version":1,"settings":{"agent":{"max_tool_calls":6}}}"#,
        r#"{"version":2,"settings":{"codex":{"web_search":null}}}"#,
        r#"{"version":2,"settings":{"portos":{"url":"synthetic"}}}"#,
        r#"{"version":2,"settings":{"portos":{"scopes":["unknown"]}}}"#,
        r#"{"version":2,"settings":{"portos":{"allow_tools":["unknown"]}}}"#,
        r#"{"version":2,"settings":{"agent":{"max_tool_calls":6},"codex":{}}}"#,
        r#"{"version":2,"settings":{"portos":{"scopes":[]}}}"#,
        r#"{"version":2,"settings":{"portos":{"scopes":["endpoint:read","endpoint:read"]}}}"#,
        r#"{"version":2,"settings":{"home":{"agent_tools":["notes","notes"]}}}"#,
    ] {
        assert!(
            serde_json::from_str::<SettingsPatchV2>(invalid).map_or(true, |value| !value.settings.valid_shape()),
            "{invalid}"
        );
        writeln!(std::io::stdout().lock(), "Refused {invalid}").unwrap();
    }
    let clear = r#"{"version":2,"settings":{"codex":{"delegate_model":"","delegate_effort":""},"home":{"agent_tools":[]},"portos":{"allow_tools":[]}}}"#;
    let patch = serde_json::from_str::<SettingsPatchV2>(clear)?;
    assert!(patch.settings.valid_shape());
    round_trip(&patch, clear)
}

#[test]
fn settings_v2_snapshot_round_trips_public_values() -> Result<(), Box<dyn Error>> {
    use echo_screen_contract::{settings_v1::SettingsSnapshotV1, settings_v2::SettingsSnapshotV2};

    let wire = r#"{
        "version":2,
        "settings":{
            "voice":{"session_listen_enabled":true,"session_listen_seconds":15},
            "codex":{"voice":"marin","web_search":"cached","delegate_model":"synthetic-runner","delegate_effort":"low"},
            "agent":{"daily_request_cap":"9223372036854775807","max_tool_calls":6,"task_deadline_seconds":45},
            "home":{"agent_tools":["notes","prompts"]}
        },
        "allowed_voices":["default","marin"],
        "prompts":{"voice":"synthetic voice","runner":"synthetic runner"},
        "portos":{
            "link_status":"relink_for_scope","operation_state":"idle","link_waiting_for_restart":true,
            "scope_choices":["endpoint:read","message:send"],"tool_choices":["endpoint_query","control"],
            "server":{
                "name":"portos","url":"http://127.0.0.1/mcp","requested_scopes":["endpoint:read","message:send"],
                "granted_scopes":["endpoint:read"],"allow_tools":["endpoint_query","control"]
            }
        },
        "bridge_version":"synthetic","restart_pending":true,"warnings":[]
    }"#;
    let snapshot = serde_json::from_str::<SettingsSnapshotV2>(wire)?;
    round_trip(&snapshot, wire)?;
    assert!(serde_json::from_str::<SettingsSnapshotV1>(wire).is_err());
    writeln!(std::io::stdout().lock(), "{}", serde_json::to_string(&snapshot)?)?;
    Ok(())
}
