use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

use futures_util::FutureExt as _;
use serde_json::json;

use crate::{
    link::OsRandom,
    tests::{ManualClock, demo_task},
};

use super::*;

fn open(path: &Path, clock: &ManualClock) -> Tasks {
    Tasks::open(path, clock.clone(), OsRandom::new()).unwrap()
}

fn admit(tasks: &Tasks) -> Submission {
    let task = tasks.prepare(&demo_task()).unwrap();
    tasks.admit(&task).unwrap();
    task
}

fn report(id: &TaskId) -> Report {
    Report {
        task_id: id.clone(),
        status: ReportStatus::Completed,
        summary: "Finished".into(),
    }
}

#[tokio::test]
async fn t1_task_deadline_arithmetic_and_terminal_races() {
    let root = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(u64::MAX - 1);
    let tasks = open(&root.path().join("tasks.json"), &clock);
    assert!(matches!(
        tasks.prepare(&demo_task()),
        Err(ServiceError::InvalidDeadline)
    ));
    let mut task = demo_task();
    task.deadline_seconds = 0;
    assert!(matches!(tasks.prepare(&task), Err(ServiceError::InvalidDeadline)));

    clock.set(4);
    let complete = admit(&tasks).id;
    let expired = admit(&tasks).id;
    clock.set(5);
    assert_eq!(tasks.report(report(&complete)), Ok(()));
    assert!(tasks.next_event(&expired).now_or_never().is_none());
    clock.set(6);
    assert_eq!(tasks.report(report(&expired)), Err(ServiceError::UnknownTask));
    assert_eq!(
        tasks.next_event(&complete).await,
        Ok(Some(TaskEvent::Completed {
            task_id: complete.clone(),
            summary: "Finished".into()
        }))
    );
    assert_eq!(
        tasks.next_event(&expired).await,
        Ok(Some(TaskEvent::Failed {
            task_id: expired.clone(),
            retryable: false,
            message: TIMEOUT.into()
        }))
    );
    assert_eq!(tasks.next_event(&expired).await, Ok(None));
}

#[tokio::test]
async fn t1_task_cancel_and_report_are_mutually_exclusive() {
    let root = tempfile::tempdir().unwrap();
    let tasks = open(&root.path().join("tasks.json"), &ManualClock::new(1));
    let cancelled = admit(&tasks);
    let completed = admit(&tasks);
    assert_eq!(tasks.progress(&cancelled.id, "queued".into()), Ok(true));
    assert_eq!(tasks.cancel(&cancelled.id), Ok(Some(cancelled.session)));
    assert_eq!(tasks.report(report(&cancelled.id)), Err(ServiceError::UnknownTask));
    assert_eq!(tasks.progress(&cancelled.id, "late".into()), Ok(false));
    assert_eq!(tasks.next_event(&cancelled.id).await, Ok(None));
    assert_eq!(tasks.report(report(&completed.id)), Ok(()));
    assert_eq!(tasks.cancel(&completed.id), Ok(None));
    assert_eq!(
        tasks.next_event(&completed.id).await,
        Ok(Some(TaskEvent::Completed {
            task_id: completed.id,
            summary: "Finished".into()
        }))
    );
}

#[test]
fn t1_task_report_requires_exact_fields() {
    let good = json!({"task_id":"task-1","status":"completed","summary":"Finished"});
    assert_eq!(
        serde_json::from_value::<Report>(good.clone()).unwrap().task_id,
        TaskId::new("task-1").unwrap()
    );
    for (key, value) in [
        ("consent", json!(true)),
        ("status", json!("running")),
        ("task_id", json!("")),
        ("summary", json!(null)),
    ] {
        let mut bad = good.clone();
        bad[key] = value;
        assert!(serde_json::from_value::<Report>(bad).is_err(), "{key}");
    }
    let mut missing = good;
    missing.as_object_mut().unwrap().remove("summary");
    assert!(serde_json::from_value::<Report>(missing).is_err());
}

#[tokio::test]
async fn t1_task_failed_commits_preserve_pending_state() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("tasks.json");
    let aside = root.path().join("aside.json");
    let clock = ManualClock::new(1);
    let tasks = open(&path, &clock);
    let first = admit(&tasks);
    let second = tasks.prepare(&demo_task()).unwrap();
    block_rename(&path, &aside);
    assert_eq!(tasks.admit(&second), Err(ServiceError::Storage));
    assert_eq!(tasks.report(report(&first.id)), Err(ServiceError::Storage));
    assert_eq!(tasks.next_event(&second.id).await, Err(ServiceError::UnknownTask));
    assert!(tasks.next_event(&first.id).now_or_never().is_none());
    restore(&path, &aside);
    assert_eq!(tasks.report(report(&first.id)), Ok(()));
    block_rename(&path, &aside);
    assert_eq!(tasks.next_event(&first.id).await, Err(ServiceError::Storage));
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 3);
    restore(&path, &aside);
    drop(tasks);
    let tasks = open(&path, &clock);
    assert_eq!(
        tasks.next_event(&first.id).await,
        Ok(Some(TaskEvent::Completed {
            task_id: first.id.clone(),
            summary: "Finished".into()
        }))
    );
    assert_eq!(tasks.next_event(&first.id).await, Ok(None));
    assert_eq!(tasks.report(report(&first.id)), Err(ServiceError::UnknownTask));
    assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o600);
}

fn block_rename(path: &Path, aside: &Path) {
    fs::rename(path, aside).unwrap();
    fs::create_dir(path).unwrap();
}

fn restore(path: &Path, aside: &Path) {
    fs::remove_dir(path).unwrap();
    fs::rename(aside, path).unwrap();
}
