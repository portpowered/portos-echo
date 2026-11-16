use super::{
    api::authorized,
    config::{ClockConfig, Config},
    ipc::parse_response,
    state::{STORE_RECORDS, ServiceClock, ServiceStore, quota},
};
use crate::{Clock, Snapshot, TaskStore};
use axum::http::{HeaderMap, HeaderValue};
use echo_contracts::secret::Secret;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

#[test]
fn exact_single_bearer_required() {
    let credential = Secret::new(vec![b'x'; 32]);
    let mut headers = HeaderMap::new();
    assert!(!authorized(&headers, &credential));
    for value in ["Basic x", "Bearer x", "bearer x", "Bearer ", "Bearer x x"] {
        headers.insert("authorization", HeaderValue::from_str(value).unwrap());
        assert!(!authorized(&headers, &credential));
    }
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {}", "x".repeat(32))).unwrap(),
    );
    assert!(authorized(&headers, &credential));
    headers.append("authorization", HeaderValue::from_static("Bearer x"));
    assert!(!authorized(&headers, &credential));
}

#[test]
fn envelope_never_accepts_wrong_identity_or_shape() {
    let input = br#"{"task_id":"task-x"}"#;
    assert_eq!(
        parse_response(input, br#"{"version":1,"output":{"accepted":true,"task_id":"task-x"}}"#).unwrap(),
        r#"{"accepted":true,"task_id":"task-x"}"#
    );
    for value in [
        json!({"version":2,"output":{"accepted":true,"task_id":"task-x"}}),
        json!({"version":1,"output":{"accepted":true,"task_id":"task-y"}}),
        json!({"version":1,"output":{"accepted":false,"task_id":"task-x"}}),
        json!({"version":1,"output":{"accepted":true,"task_id":"task-x","extra":1}}),
        json!({"version":1,"output":{"accepted":true,"task_id":"task-x"},"error":"x"}),
        json!({"version":1,"extra":"x"}),
        json!({"version":1,"error":null}),
    ] {
        assert_eq!(
            parse_response(input, value.to_string().as_bytes()),
            Err("invalid report response".into())
        );
    }
    assert_eq!(
        parse_response(input, br#"{"version":1,"error":"cancelled task_id"}"#),
        Err("cancelled task_id".into())
    );
}

#[test]
fn clock_is_monotonic_and_system_clock_cannot_be_overridden() {
    let clock = ServiceClock::new(&ClockConfig::ControlledStdin {
        initial_unix_seconds: 100,
    });
    assert_eq!(clock.now(), 100);
    clock.set(160).unwrap();
    assert_eq!(clock.now(), 160);
    assert_eq!(clock.set(159), Err("clock_moved_backwards"));
    assert_eq!(
        ServiceClock::new(&ClockConfig::System).set(1),
        Err("clock_control_unavailable")
    );
}

#[test]
fn transaction_fault_latches_health_and_rolls_back() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = temp.path().join("tasks.json");
    let store = ServiceStore::open(&path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let result: Result<(), _> = store.transaction(|snapshot| {
        snapshot.version = 9;
        Err(crate::StoreError::Invalid)
    });
    assert!(result.is_err());
    assert!(!store.healthy());
    assert!(store.transaction(|_| Ok(())).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    drop(store);
    assert!(ServiceStore::open(&path).unwrap().healthy());
}

#[test]
fn quota_preserves_all_tombstones_and_config_rejects_extra_fields() {
    let mut snapshot = Snapshot::default();
    assert!(quota(&snapshot).is_ok());
    let backend_record: crate::Record = serde_json::from_value(json!({
        "task_id":"x", "task":{"delegation_id":"d","instructions":"x","context":{},"deadline_seconds":1,"max_tool_calls":1},
        "session_id":"11111111-1111-4111-8111-111111111111","deadline_unix_seconds":1,"state":"cancelled","events":[]
    })).unwrap();
    snapshot.records = vec![backend_record; STORE_RECORDS + 1];
    assert!(quota(&snapshot).is_err());
    assert_eq!(snapshot.records.len(), STORE_RECORDS + 1);
    snapshot.records.truncate(1);
    snapshot.records[0].task.instructions = "x".repeat(usize::try_from(super::state::STORE_BYTES).unwrap());
    assert!(quota(&snapshot).is_err());
    assert!(serde_json::from_value::<Config>(json!({"listen":"127.0.0.1:0", "extra":1})).is_err());
}

#[test]
fn duplicate_envelope_keys_are_protocol_errors() {
    for bytes in [
        br#"{"version":1,"version":1,"output":{"accepted":true,"task_id":"x"}}"#.as_slice(),
        br#"{"version":1,"output":{"accepted":false,"accepted":true,"task_id":"x"}}"#.as_slice(),
    ] {
        assert_eq!(
            parse_response(br#"{"task_id":"x"}"#, bytes),
            Err("invalid report response".into())
        );
    }
}

#[tokio::test(start_paused = true)]
async fn response_write_deadline_is_independent_of_reader_progress() {
    use tokio::io::AsyncWriteExt;
    let (stream, _peer) = tokio::io::duplex(1);
    let mut stream = super::transport::ResponseDeadline::new(stream);
    stream.flush().await.unwrap();
    tokio::time::advance(std::time::Duration::from_secs(3)).await;
    stream.write_all(b"x").await.unwrap();
    let result = stream.write_all(b"too large for unread peer").await;
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
}

#[test]
fn store_with_lock_extension_still_has_a_distinct_exclusive_lease() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = temp.path().join("tasks.lock");
    let store = ServiceStore::open(&path).unwrap();
    assert!(ServiceStore::open(&path).is_err());
    assert_eq!(
        serde_json::from_slice::<Snapshot>(&std::fs::read(&path).unwrap()).unwrap(),
        Snapshot::default()
    );
    drop(store);
    assert!(ServiceStore::open(&path).is_ok());
}
