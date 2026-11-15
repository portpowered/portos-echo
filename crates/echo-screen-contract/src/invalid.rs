//! T1: malformed input fails with a meaningful Serde category and diagnostic.
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
