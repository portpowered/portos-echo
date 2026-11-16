#![cfg(test)]
use echo_agent_api::{AgentBackend, AgentError};
use echo_muse_bridge::{Clock, EpochClock, JsonFileStore, MuseBackend, StoreError, TaskStore};
use serde_json::{Value, json};

use crate::tests::support::{FakeClock, RecordingGadget, task};

#[tokio::test]
async fn missing_parent_write_and_unreadable_snapshot_fail_without_admission_or_send() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing").join("tasks.json");
    let store = JsonFileStore::open(&path).unwrap();
    let gadget = RecordingGadget::default();
    let backend = MuseBackend::new(gadget.clone(), FakeClock::default(), store);
    assert_eq!(backend.submit(task()).await, Err(AgentError::UnscriptedTask));
    assert_eq!(*gadget.calls.lock().unwrap(), vec![]);
    assert!(!path.exists());
    assert!(matches!(JsonFileStore::open(directory.path()), Err(StoreError::Io(_))));
}

#[tokio::test]
async fn snapshot_rejects_inconsistent_terminal_queues_without_replacing_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tasks.json");
    let backend = MuseBackend::new(
        RecordingGadget::default(),
        FakeClock::default(),
        JsonFileStore::open(&path).unwrap(),
    );
    let id = backend.submit(task()).await.unwrap();
    drop(backend);
    let original: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let terminal = json!({"type": "completed", "task_id": id, "summary": "light is on"});
    for (state, events) in [
        ("live", json!([terminal])),
        ("cancelled", original["tasks"][0]["events"].clone()),
        ("completed", original["tasks"][0]["events"].clone()),
        ("failed", json!([terminal])),
        ("completed", json!([terminal, terminal])),
        (
            "failed",
            json!([{"type":"failed", "task_id": id, "retryable": true, "message":"x"}]),
        ),
        (
            "expired",
            json!([{"type":"failed", "task_id": id, "retryable": false, "message":"wrong"}]),
        ),
    ] {
        let mut bad = original.clone();
        bad["tasks"][0]["state"] = json!(state);
        bad["tasks"][0]["events"] = events;
        let bytes = bad.to_string();
        std::fs::write(&path, &bytes).unwrap();
        assert!(matches!(JsonFileStore::open(&path), Err(StoreError::Invalid)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
    }
    std::fs::write(&path, original.to_string()).unwrap();
    let store = JsonFileStore::open(&path).unwrap();
    assert_eq!(store.transaction(|snapshot| Ok(snapshot.records.len())).unwrap(), 1);
}

#[tokio::test]
async fn production_clock_uses_epoch_and_past_deadline_is_already_ready() {
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let clock = EpochClock;
    let now = clock.now();
    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!((before..=after).contains(&now));
    clock.wait_until(0).await;
}
