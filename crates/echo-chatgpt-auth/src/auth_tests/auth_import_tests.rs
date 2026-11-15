//! T1: explicit synthetic files only; no HTTP, home discovery or real credentials.
use super::auth_support::*;
use crate::{AuthError, AuthSource, auth_store};
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicU64},
};

fn source(path: &Path) -> AuthSource {
    let mut source = AuthSource::new(echo_config::ChatGptAuthConfig::default(), path.to_owned()).unwrap();
    source.clock = Arc::new(FakeClock(AtomicU64::new(0)));
    source
}
fn codex() -> serde_json::Value {
    serde_json::json!({
        "tokens": {"access_token":token(Some("synthetic-account")),
            "refresh_token":"synthetic-refresh", "account_id":"synthetic-account",
            "id_token":"synthetic-unused"},
        "last_refresh":"2033-05-18T03:33:20Z", "OPENAI_API_KEY":null,
    })
}
#[tokio::test]
async fn import_exact_copy_reload_and_second_import_refusal() {
    let directory = directory();
    let original = directory.0.join("codex.json");
    let bytes = format!("  {}\n", serde_json::to_string_pretty(&codex()).unwrap());
    std::fs::write(&original, &bytes).unwrap();
    let source = source(&directory.0.join("own.json"));
    let result = source.import_codex(&original).await.unwrap();
    assert_eq!(*result.access_token.expose(), token(Some("synthetic-account")));
    assert_eq!(result.account_id, "synthetic-account");
    let stored = auth_store::load(&source.file).unwrap();
    assert_eq!(stored.last_refresh, NOW);
    assert_eq!(stored.expires, NOW + 3600);
    assert_eq!(stored.refresh.expose(), "synthetic-refresh");
    let own_bytes = std::fs::read(&source.file).unwrap();
    let object: serde_json::Value = serde_json::from_slice(&own_bytes).unwrap();
    assert_eq!(object.as_object().unwrap().len(), 5);
    assert_eq!(
        *source.current().await.unwrap().access_token.expose(),
        token(Some("synthetic-account"))
    );
    assert_eq!(
        source.import_codex(&original).await.err(),
        Some(AuthError::AlreadyImported)
    );
    assert_eq!(
        source.import_codex(&source.file).await.err(),
        Some(AuthError::AlreadyImported)
    );
    assert_eq!(std::fs::read(&source.file).unwrap(), own_bytes);
    assert_eq!(std::fs::read(&original).unwrap(), bytes.as_bytes());
}
#[tokio::test]
async fn import_rejects_invalid_exp_account_timestamp_and_missing_source() {
    let directory = directory();
    let original = directory.0.join("codex.json");
    let source = source(&directory.0.join("own.json"));
    assert_eq!(source.import_codex(&original).await.err(), Some(AuthError::NotSignedIn));
    let mut mismatch = codex();
    mismatch["tokens"]["account_id"] = "other".into();
    let mut missing = codex();
    missing["tokens"]["access_token"] = token(None).into();
    let mut malformed = codex();
    malformed["tokens"]["access_token"] = "bad".into();
    let mut blank = codex();
    blank["tokens"]["refresh_token"] = "".into();
    let mut date = codex();
    date["last_refresh"] = "2033-02-30T03:33:20Z".into();
    for (value, error) in [
        (mismatch, AuthError::AccountMismatch),
        (missing, AuthError::AccountIdMissing),
        (malformed, AuthError::InvalidCredential),
        (blank, AuthError::InvalidCredential),
        (date, AuthError::InvalidCredential),
        (serde_json::json!({}), AuthError::InvalidCredential),
    ] {
        let bytes = value.to_string();
        std::fs::write(&original, &bytes).unwrap();
        assert_eq!(source.import_codex(&original).await.err(), Some(error));
        assert!(!source.file.exists());
        assert_eq!(std::fs::read(&original).unwrap(), bytes.as_bytes());
    }
    for bytes in [b"not json".to_vec(), vec![b' '; 262_145]] {
        std::fs::write(&original, &bytes).unwrap();
        assert_eq!(
            source.import_codex(&original).await.err(),
            Some(AuthError::InvalidCredential)
        );
        assert_eq!(std::fs::read(&original).unwrap(), bytes);
        assert!(!source.file.exists());
    }
}
#[tokio::test]
async fn import_timestamp_offsets_missing_metadata_and_racing_callers() {
    let directory = directory();
    let original = directory.0.join("codex.json");
    let mut value = codex();
    value["last_refresh"] = "2033-05-18T04:33:20.123+01:00".into(); // lint-rules: allow-literal inert RFC3339 timestamp
    std::fs::write(&original, value.to_string()).unwrap();
    let first = source(&directory.0.join("own.json"));
    let second = source(&first.file);
    let (a, b) = tokio::join!(first.import_codex(&original), second.import_codex(&original));
    assert_eq!(a.unwrap().account_id, "synthetic-account");
    assert_eq!(b.err(), Some(AuthError::AlreadyImported));
    assert_eq!(auth_store::load(&first.file).unwrap().last_refresh, NOW);
    value.as_object_mut().unwrap().remove("last_refresh");
    std::fs::write(&original, value.to_string()).unwrap();
    let third = source(&directory.0.join("without-age.json"));
    third.import_codex(&original).await.unwrap();
    let stored = auth_store::load(&third.file).unwrap();
    assert_eq!(stored.last_refresh, 0);
    assert!(stored.due(NOW));
}
