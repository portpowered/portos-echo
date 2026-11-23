use super::*;

#[test]
fn openai_recovery_messages_round_trip() -> Result<(), Box<dyn Error>> {
    let uri = "https://example.invalid/verify"; // lint-rules: allow-literal wire serialization fixture
    let signin_wire = r#"{"state":"open_ai_signin","attempt_id":"attempt-1","user_code":"DEMO-CODE","verification_uri":"https://example.invalid/verify","expires_at_unix":1800000300,"muted":true}"#; // lint-rules: allow-literal wire serialization fixture
    let relink = UiState::OpenAiRelink {
        message: "Relink to sign in to ChatGPT.".into(),
        starting: false,
        muted: true,
    };
    let signin = UiState::OpenAiSignin {
        attempt_id: "attempt-1".into(),
        user_code: "DEMO-CODE".into(),
        verification_uri: uri.into(),
        expires_at_unix: 1_800_000_300,
        muted: true,
    };
    for (state, wire) in [
        (
            relink,
            r#"{"state":"open_ai_relink","message":"Relink to sign in to ChatGPT.","starting":false,"muted":true}"#,
        ),
        (signin, signin_wire),
    ] {
        round_trip(&state, wire)?;
        round_trip(
            &ScreenFrame { seq: 7, state },
            &format!(r#"{{"seq":7,"state":{wire}}}"#),
        )?;
    }
    round_trip(
        &UiAction::CancelOpenAiSignin {
            attempt_id: "attempt-1".into(),
        },
        r#"{"action":"cancel_open_ai_signin","attempt_id":"attempt-1"}"#,
    )?;
    round_trip(
        &UiAction::RenewOpenAiSignin {
            attempt_id: "attempt-1".into(),
        },
        r#"{"action":"renew_open_ai_signin","attempt_id":"attempt-1"}"#,
    )
}

#[test]
fn openai_expiry_preserves_unsigned_bounds() -> Result<(), Box<dyn Error>> {
    for expires_at_unix in [0, u64::MAX] {
        preserved(&UiState::OpenAiSignin {
            attempt_id: "attempt-1".into(),
            user_code: "DEMO-CODE".into(),
            verification_uri: String::new(),
            expires_at_unix,
            muted: false,
        })?;
    }
    Ok(())
}
