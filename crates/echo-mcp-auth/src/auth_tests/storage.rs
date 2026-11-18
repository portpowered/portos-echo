use super::*;
use crate::store::Store;
use serde_json::{Value, json};

#[tokio::test(start_paused = true)]
async fn t3_commit_before_return_survives_termination_and_reload() {
    let h = Harness::linked().await;
    let original = h.record();
    let (committed, wait) = tokio::sync::oneshot::channel();
    let task = tokio::spawn({
        let c = h.client.clone();
        async move {
            let mut state = c.state.lock().await;
            // Production rotation is the same operation force_refresh awaits before
            // reading the access value. Terminate exactly at this internal boundary.
            c.rotate(&mut state).await.unwrap();
            committed.send(()).unwrap();
            std::future::pending::<()>().await;
            token(&state)
        }
    });
    drive(wait).await.unwrap();
    task.abort();
    assert!(matches!(drive(task).await, Err(e) if e.is_cancelled()));
    let reloaded = h.reload().await.unwrap();
    assert_eq!(h.record()["private_key_jwk"], original["private_key_jwk"]);
    assert_eq!(drive(reloaded.force_refresh()).await.unwrap().expose(), "access-3");
    assert_eq!(h.record()["grant"]["refresh_token"], "refresh-3");
    let state = h.mock.state.lock().unwrap();
    let used = state
        .requests
        .iter()
        .filter_map(|r| r.form.get("refresh_token"))
        .collect::<Vec<_>>();
    assert_eq!(used, ["refresh-1", "refresh-2"]);
}

#[tokio::test(start_paused = true)]
async fn t3_store_replace_failure_blocks_access_and_never_replays() {
    let h = Harness::linked().await;
    let original = h.record();
    let path = h.directory.path().join("grant.json");
    let bytes = std::fs::read(&path).unwrap();
    let backup = h.directory.path().join("previous.json");
    std::fs::rename(&path, &backup).unwrap();
    std::fs::create_dir(&path).unwrap();
    error(drive(h.client.force_refresh()).await, AuthError::Storage);
    error(drive(h.client.access_token()).await, AuthError::RelinkRequired);
    error(drive(h.client.force_refresh()).await, AuthError::RelinkRequired);
    assert_eq!(h.mock.refreshes(), 1);
    let retained_bytes = std::fs::read(backup).unwrap();
    assert_eq!(retained_bytes, bytes);
    let retained: Value = serde_json::from_slice(&retained_bytes).unwrap();
    assert_eq!(retained, original);
}

#[test]
fn t2_private_store_schema_roundtrip_and_invalid_versions_or_keys() {
    let schema: Value = serde_json::from_str(include_str!("../../data/auth-store.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mock_options = AuthOptions {
        resource_url: "https://example.invalid/mcp".into(), // lint-rules: allow-literal store identity fixture serialized locally without network access
        client_id: "synthetic-cli".into(),
        scopes: vec![],
        storage_dir: dir.path().into(),
        request_timeout: Duration::from_secs(1),
    };
    let (store, mut record) = Store::open(&mock_options, "https://example.invalid").unwrap(); // lint-rules: allow-literal store identity fixture serialized locally without network access
    let load = || -> Value { serde_json::from_slice(&std::fs::read(dir.path().join("grant.json")).unwrap()).unwrap() };
    let unlinked = load();
    assert!(validator.is_valid(&unlinked));
    assert_eq!(unlinked.as_object().unwrap().len(), 6);
    assert_eq!(unlinked["version"], 1);
    assert_eq!(unlinked["grant"], json!(null));
    record.grant = Some(Secret::new(crate::store::Grant {
        access_token: "synthetic-access".into(),
        refresh_token: "synthetic-refresh".into(),
        expires_at_unix: 42,
        scope: "endpoint:read".into(),
    }));
    store.save(&record).unwrap();
    let linked = load();
    assert!(validator.is_valid(&linked));
    let (_, reloaded) = Store::open(&mock_options, "https://example.invalid").unwrap(); // lint-rules: allow-literal store identity fixture serialized locally without network access
    assert_eq!(reloaded.grant.unwrap().expose().refresh_token, "synthetic-refresh");
    assert_eq!(linked["private_key_jwk"], unlinked["private_key_jwk"]);
    assert_eq!(
        linked["grant"],
        json!({"access_token":"synthetic-access","refresh_token":"synthetic-refresh","expires_at_unix":42,"scope":"endpoint:read"})
    );
    for bad in [json!({"version":2}), json!({"private_key_jwk":{"kty":"EC"}})] {
        let mut value = linked.clone();
        for (key, entry) in bad.as_object().unwrap() {
            value[key] = entry.clone();
        }
        assert!(!validator.is_valid(&value));
        let bytes = serde_json::to_vec(&value).unwrap();
        std::fs::write(dir.path().join("grant.json"), &bytes).unwrap();
        error(
            Store::open(&mock_options, "https://example.invalid"), // lint-rules: allow-literal store identity fixture serialized locally without network access
            AuthError::Storage,
        );
        assert_eq!(std::fs::read(dir.path().join("grant.json")).unwrap(), bytes);
    }
}

#[test]
fn corrupt_and_mismatched_records_are_never_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let mut options = AuthOptions {
        resource_url: "https://example.invalid/mcp".into(), // lint-rules: allow-literal store identity fixture serialized locally without network access
        client_id: "synthetic-cli".into(),
        scopes: vec![],
        storage_dir: dir.path().into(),
        request_timeout: Duration::from_secs(1),
    };
    Store::open(&options, "https://example.invalid").unwrap(); // lint-rules: allow-literal store identity fixture serialized locally without network access
    let path = dir.path().join("grant.json");
    let before = std::fs::read(&path).unwrap();
    options.client_id = "another-client".into();
    error(
        Store::open(&options, "https://example.invalid"), // lint-rules: allow-literal store identity fixture serialized locally without network access
        AuthError::RelinkRequired,
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    std::fs::write(&path, b"corrupt json").unwrap();
    error(Store::open(&options, "https://example.invalid"), AuthError::Storage); // lint-rules: allow-literal store identity fixture serialized locally without network access
    assert_eq!(std::fs::read(&path).unwrap(), b"corrupt json");
}

#[cfg(unix)]
#[test]
fn private_replacement_files_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let options = AuthOptions {
        resource_url: "https://example.invalid/mcp".into(), // lint-rules: allow-literal store identity fixture serialized locally without network access
        client_id: "synthetic-cli".into(),
        scopes: vec![],
        storage_dir: dir.path().into(),
        request_timeout: Duration::from_secs(1),
    };
    let (store, record) = Store::open(&options, "https://example.invalid").unwrap(); // lint-rules: allow-literal store identity fixture serialized locally without network access
    store.save(&record).unwrap();
    assert_eq!(
        std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(dir.path().join("grant.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[tokio::test(start_paused = true)]
async fn t3_refresh_clock_skew_preserves_rotation_and_storage_failure() {
    for storage_failure in [false, true] {
        let h = Harness::linked().await;
        let original = h.record();
        let path = h.directory.path().join("grant.json");
        let backup = h.directory.path().join("previous.json");
        let bytes = std::fs::read(&path).unwrap();
        if storage_failure {
            std::fs::rename(&path, &backup).unwrap();
            std::fs::create_dir(&path).unwrap();
        }
        let before = h.client.now();
        h.mock.state.lock().unwrap().raw_wall = Some(1_800_000_010);
        if storage_failure {
            error(drive(h.client.force_refresh()).await, AuthError::Storage);
            error(drive(h.client.access_token()).await, AuthError::RelinkRequired);
            error(drive(h.client.force_refresh()).await, AuthError::RelinkRequired);
            assert_eq!(std::fs::read(backup).unwrap(), bytes);
        } else {
            assert_eq!(drive(h.client.force_refresh()).await.unwrap().expose(), "access-2");
            assert_eq!(h.record()["grant"]["refresh_token"], "refresh-2");
            assert_eq!(h.record()["private_key_jwk"], original["private_key_jwk"]);
        }
        assert_eq!(h.client.now(), before);
        assert_eq!(h.mock.refreshes(), 1);
    }
}
