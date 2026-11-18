use super::auth_support::*;
use crate::{AuthError, AuthSource, auth_store};

#[test]
fn store_roundtrip_and_atomic_replacement_preserve_five_fields() {
    let directory = directory();
    let staging = crate::auth_private::Directory::new(&directory.0).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&staging.0).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    drop(staging);
    let path = directory.0.join("credential.json");
    let mut first = record();
    auth_store::save(&path, &first).unwrap();
    let loaded = auth_store::load(&path).unwrap();
    assert_eq!(*loaded.access.expose(), token(Some("synthetic-account")));
    assert_eq!(loaded.refresh.expose(), "synthetic-refresh");
    assert_eq!(loaded.expires, NOW + 3600);
    assert_eq!(loaded.account_id, "synthetic-account");
    assert_eq!(loaded.last_refresh, NOW);
    first.refresh = echo_contracts::secret::Secret::new("synthetic-rotated".into());
    auth_store::save(&path, &first).unwrap();
    assert_eq!(auth_store::load(&path).unwrap().refresh.expose(), "synthetic-rotated");
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
    check_permissions(&path);
}
#[cfg(unix)]
fn check_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o600);
}
#[test]
fn store_invalid_missing_oversized_and_write_failure_are_safe() {
    let directory = directory();
    let path = directory.0.join("credential.json");
    assert_eq!(auth_store::load(&path).err(), Some(AuthError::NotSignedIn));
    for bytes in [b"INJECTED_VALUE".to_vec(), vec![b' '; 262_145]] {
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(auth_store::load(&path).err(), Some(AuthError::InvalidCredential));
    }
    auth_store::save(&path, &record()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let mut mismatch = record();
    mismatch.account_id = "wrong-account".into();
    assert_eq!(auth_store::save(&path, &mismatch), Err(AuthError::AccountMismatch));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let missing = directory.0.join("absent").join("auth");
    assert_eq!(auth_store::save(&missing, &record()), Err(AuthError::Storage));
    let blocked = directory.0.join("blocked");
    std::fs::create_dir(&blocked).unwrap();
    assert_eq!(auth_store::save(&blocked, &record()), Err(AuthError::Storage));
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 2);
}
#[tokio::test]
async fn store_current_refuses_account_mismatch_before_network() {
    let directory = directory();
    let path = directory.0.join("auth");
    let mut source = AuthSource::new(echo_config::ChatGptAuthConfig::default(), path.clone()).unwrap();
    source.clock = std::sync::Arc::new(FakeClock(std::sync::atomic::AtomicU64::new(0)));
    let mut json = serde_json::to_value(record()).unwrap();
    json["account_id"] = "wrong".into();
    std::fs::write(&path, json.to_string()).unwrap();
    assert_eq!(source.current().await.err(), Some(AuthError::AccountMismatch));
}
#[test]
fn store_permission_write_and_rename_faults_preserve_old_bytes() {
    let directory = directory();
    let path = directory.0.join("credential");
    auth_store::save(&path, &record()).unwrap();
    let before = std::fs::read(&path).unwrap();
    for stage in 0..3 {
        let result = auth_store::save_with(&path, &record(), |at| {
            if at == stage { Err(AuthError::Storage) } else { Ok(()) }
        });
        assert_eq!(result, Err(AuthError::Storage));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
    }
}
#[test]
fn store_refresh_predicates_at_each_boundary() {
    let mut r = record();
    assert!(!r.due(NOW + 3299));
    assert!(r.due(NOW + 3300));
    r.expires = NOW + 1_000_000;
    assert!(!r.due(NOW + 691_199));
    assert!(!r.due(NOW + 691_200));
    assert!(r.due(NOW + 691_201));
    assert!(!r.due(NOW - 1));
    assert!(r.due(u64::MAX));
}
