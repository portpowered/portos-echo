use super::auth_support::*;
use crate::{AuthError, auth_token};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

#[test]
fn claims_values_missing_malformed_and_bounds() {
    assert_eq!(
        auth_token::claims(&token(Some("synthetic-account"))).unwrap(),
        ("synthetic-account".into(), NOW + 3600)
    );
    assert_eq!(auth_token::claims(&token(None)), Err(AuthError::AccountIdMissing));
    for t in [
        String::new(),
        "a.b".into(),
        "a.b.c.d".into(),
        ".b.c".into(),
        "a.b.".into(),
        "a.!!.c".into(),
        "a.e30.c".into(),
        "x".repeat(262_145),
    ] {
        assert_eq!(auth_token::claims(&t), Err(AuthError::InvalidCredential));
    }
    for value in [
        serde_json::json!({"exp":0}),
        serde_json::json!({"exp":NOW,"https://api.openai.com/auth":{"chatgpt_account_id":""}}), // lint-rules: allow-literal inert JWT claim key
    ] {
        let token = format!("e30.{}.sig", URL_SAFE_NO_PAD.encode(value.to_string()));
        assert!(matches!(
            auth_token::claims(&token),
            Err(AuthError::InvalidCredential | AuthError::AccountIdMissing)
        ));
    }
    for value in ["", " ", "a\nb"] {
        assert_eq!(auth_token::nonempty(value), Err(AuthError::InvalidCredential));
    }
}
#[test]
fn token_response_requires_both_tokens_valid_expiry_and_claim() {
    for value in [
        serde_json::json!({}),
        serde_json::json!({"access_token":"bad","refresh_token":"test"}),
        serde_json::json!({"access_token":token(Some("test")),"refresh_token":""}),
        serde_json::json!({"access_token":token(Some("test")),"refresh_token":"test","expires_in":0}),
        serde_json::json!({"access_token":token(Some("test")),"refresh_token":"test","expires_in":u64::MAX}),
    ] {
        assert_eq!(
            auth_token::response(value.to_string().as_bytes(), NOW).err(),
            Some(AuthError::InvalidCredential)
        );
    }
    let r = auth_token::response(
        serde_json::json!({"access_token":token(Some("test")),"refresh_token":"test"})
            .to_string()
            .as_bytes(),
        NOW,
    )
    .unwrap();
    assert_eq!(r.expires, NOW + 3600);
    assert_eq!(
        auth_token::response(tokens(Some("test")).as_bytes(), NOW + 3600).err(),
        Some(AuthError::InvalidCredential)
    );
}
