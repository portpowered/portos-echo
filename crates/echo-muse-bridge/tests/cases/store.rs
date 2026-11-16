#![cfg(test)]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use echo_agent_api::{AgentBackend, AgentError};
use echo_muse_bridge::{InMemoryStore, JsonFileStore, MuseBackend, Snapshot, StoreError, TaskStore};
use serde_json::{Value, json};

use crate::support::{FakeClock, RecordingGadget, accepted, completed, progress, report, task, timeout};

#[tokio::test]
async fn live_file_reopens_and_event_consumption_and_tombstones_are_durable() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tasks.json");
    let gadget = RecordingGadget::default();
    let clock = FakeClock::default();
    clock.set(1000);
    let a = MuseBackend::new(gadget.clone(), clock.clone(), JsonFileStore::open(&path).unwrap());
    let id = a.submit(task()).await.unwrap();
    let bytes: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(bytes["version"], 1);
    assert_eq!(bytes["tasks"][0]["task_id"], id.to_string());
    assert_eq!(bytes["tasks"][0]["deadline_unix_seconds"], 1060);
    assert_eq!(bytes["tasks"][0]["task"], serde_json::to_value(task()).unwrap());
    assert_eq!(bytes["tasks"][0]["state"], "live");
    drop(a);
    let b = MuseBackend::new(gadget.clone(), clock.clone(), JsonFileStore::open(&path).unwrap());
    assert_eq!(
        b.report_intake().accept(&report(&id, "completed")).unwrap(),
        accepted(&id)
    );
    assert_eq!(b.next_event(&id).await, Some(progress(&id)));
    assert_eq!(b.next_event(&id).await, Some(completed(&id)));
    drop(b);
    let c = MuseBackend::new(gadget.clone(), clock, JsonFileStore::open(&path).unwrap());
    assert_eq!(c.next_event(&id).await, None);
    assert_eq!(
        c.report_intake()
            .accept(&report(&id, "completed"))
            .unwrap_err()
            .to_string(),
        "task_id already reported"
    );
    assert_eq!(gadget.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn reopened_expiry_failed_and_cancelled_tombstones_never_replay() {
    for state in ["expired", "failed", "cancelled"] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tasks.json");
        let clock = FakeClock::default();
        let a = MuseBackend::new(
            RecordingGadget::default(),
            clock.clone(),
            JsonFileStore::open(&path).unwrap(),
        );
        let id = a.submit(task()).await.unwrap();
        assert_eq!(a.next_event(&id).await, Some(progress(&id)));
        match state {
            "failed" => {
                a.report_intake().accept(&report(&id, "failed")).unwrap();
            }
            "cancelled" => a.cancel(&id).await.unwrap(),
            _ => clock.set(61),
        }
        drop(a);
        let b = MuseBackend::new(
            RecordingGadget::default(),
            clock.clone(),
            JsonFileStore::open(&path).unwrap(),
        );
        let reason = match state {
            "failed" => "task_id already reported",
            "cancelled" => "cancelled task_id",
            _ => "expired task_id",
        };
        assert_eq!(
            b.report_intake()
                .accept(&report(&id, "completed"))
                .unwrap_err()
                .to_string(),
            reason
        );
        let event = b.next_event(&id).await;
        match state {
            "expired" => assert_eq!(event, Some(timeout(&id))),
            "cancelled" => assert_eq!(event, None),
            _ => assert_eq!(
                event,
                Some(echo_contracts::task::TaskEvent::Failed {
                    task_id: id.clone(),
                    retryable: false,
                    message: "light is on".into()
                })
            ),
        }
        drop(b);
        let c = MuseBackend::new(RecordingGadget::default(), clock, JsonFileStore::open(&path).unwrap());
        assert_eq!(c.next_event(&id).await, None);
        assert_eq!(
            c.report_intake()
                .accept(&report(&id, "completed"))
                .unwrap_err()
                .to_string(),
            reason
        );
    }
}

#[test]
fn empty_snapshot_roundtrips_and_corrupt_versions_and_fields_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tasks.json");
    let store = JsonFileStore::open(&path).unwrap();
    store
        .transaction(|snapshot| {
            assert_eq!(*snapshot, Snapshot::default());
            Ok(())
        })
        .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"version":1,"tasks":[]}"#);
    for bytes in [
        "{",
        r#"{"version":2,"tasks":[]}"#,
        r#"{"version":1,"tasks":[],"extra":0}"#,
    ] {
        std::fs::write(&path, bytes).unwrap();
        assert!(JsonFileStore::open(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
    }
}

#[tokio::test]
async fn duplicate_ids_unknown_fields_bad_uuid_and_state_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tasks.json");
    let backend = MuseBackend::new(
        RecordingGadget::default(),
        FakeClock::default(),
        JsonFileStore::open(&path).unwrap(),
    );
    backend.submit(task()).await.unwrap();
    drop(backend);
    let original: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for field in [
        "duplicate",
        "extra",
        "session_id",
        "state",
        "deadline_unix_seconds",
        "events",
    ] {
        let mut bad = original.clone();
        match field {
            "duplicate" => {
                let record = bad["tasks"][0].clone();
                bad["tasks"].as_array_mut().unwrap().push(record);
            }
            "extra" => bad["tasks"][0]["extra"] = json!(true),
            "deadline_unix_seconds" => bad["tasks"][0][field] = json!(-1),
            "events" => bad["tasks"][0]["events"][0]["task_id"] = json!("wrong-id"),
            _ => bad["tasks"][0][field] = json!("invalid"),
        }
        let bytes = bad.to_string();
        std::fs::write(&path, &bytes).unwrap();
        assert!(JsonFileStore::open(&path).is_err(), "{field}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
    }
}

/// Fault injection is inside the store transaction; acknowledgements require commit.
struct FaultStore {
    inner: InMemoryStore,
    fail: Arc<AtomicBool>,
}

impl TaskStore for FaultStore {
    fn transaction<R>(&self, update: impl FnOnce(&mut Snapshot) -> Result<R, StoreError>) -> Result<R, StoreError> {
        self.inner.transaction(|snapshot| {
            let result = update(snapshot)?;
            if self.fail.load(Ordering::SeqCst) {
                return Err(StoreError::Invalid);
            }
            Ok(result)
        })
    }
}

#[tokio::test]
async fn failed_store_commit_never_acknowledges_or_consumes_and_can_retry() {
    let fail = Arc::new(AtomicBool::new(true));
    let store = FaultStore {
        inner: InMemoryStore::default(),
        fail: fail.clone(),
    };
    let gadget = RecordingGadget::default();
    let backend = MuseBackend::new(gadget.clone(), FakeClock::default(), store);
    assert_eq!(backend.submit(task()).await, Err(AgentError::UnscriptedTask));
    assert_eq!(*gadget.calls.lock().unwrap(), vec![]);
    fail.store(false, Ordering::SeqCst);
    let id = backend.submit(task()).await.unwrap();
    fail.store(true, Ordering::SeqCst);
    assert_eq!(
        backend
            .report_intake()
            .accept(&report(&id, "completed"))
            .unwrap_err()
            .to_string(),
        "report persistence failed: unsupported or inconsistent store snapshot"
    );
    assert_eq!(backend.next_event(&id).await, None);
    assert_eq!(backend.cancel(&id).await, Err(AgentError::UnscriptedTask));
    fail.store(false, Ordering::SeqCst);
    assert_eq!(
        backend.report_intake().accept(&report(&id, "completed")).unwrap(),
        accepted(&id)
    );
    assert_eq!(backend.next_event(&id).await, Some(progress(&id)));
    assert_eq!(backend.next_event(&id).await, Some(completed(&id)));
}

#[tokio::test]
async fn rejected_transaction_and_failed_replace_leave_committed_bytes_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tasks.json");
    let backend = MuseBackend::new(
        RecordingGadget::default(),
        FakeClock::default(),
        JsonFileStore::open(&path).unwrap(),
    );
    backend.submit(task()).await.unwrap();
    drop(backend);
    let store = JsonFileStore::open(&path).unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(
        store
            .transaction::<()>(|snapshot| {
                snapshot.version = 2;
                Ok(())
            })
            .is_err()
    );
    assert!(
        store
            .transaction::<()>(|snapshot| {
                snapshot.records.clear();
                Err(StoreError::Invalid)
            })
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    // Preserve the committed file, then make its replacement destination a directory.
    let saved = directory.path().join("saved.json");
    std::fs::rename(&path, &saved).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(matches!(
        store.transaction::<()>(|snapshot| {
            snapshot.records[0].events.clear();
            Ok(())
        }),
        Err(StoreError::Io(_))
    ));
    assert_eq!(std::fs::read(&saved).unwrap(), before);
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(&saved, &path).unwrap();
    assert_eq!(
        store
            .transaction(|snapshot| Ok(snapshot.records[0].events.len()))
            .unwrap(),
        1
    );
    store
        .transaction(|snapshot| {
            snapshot.records[0].events.clear();
            Ok(())
        })
        .unwrap();
    assert_eq!(
        JsonFileStore::open(&path)
            .unwrap()
            .transaction(|snapshot| Ok(snapshot.records[0].events.len()))
            .unwrap(),
        0
    );
}

#[cfg(unix)]
#[tokio::test]
async fn unix_create_and_atomic_replacement_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tasks.json");
    let backend = MuseBackend::new(
        RecordingGadget::default(),
        FakeClock::default(),
        JsonFileStore::open(&path).unwrap(),
    );
    let id = backend.submit(task()).await.unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    backend.report_intake().accept(&report(&id, "completed")).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
}
