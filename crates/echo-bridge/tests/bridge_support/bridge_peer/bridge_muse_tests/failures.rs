//! T3 callback and cancel semantics through the production client and daemon.
use super::*;

async fn reject(muse: &Muse, id: &str, message: &str) {
    let output = muse
        .report_output(id, "completed", "Late success must never be spoken")
        .await;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"");
    assert!(String::from_utf8(output.stderr).unwrap().contains(message));
}

#[tokio::test]
#[ignore = "T3 requires prebuilt Muse binaries, Linux and openssl"]
async fn muse_unknown_and_duplicate_reports_cannot_complete_another_task() {
    let muse = Muse::start("subjectAltName=IP:127.0.0.1").await;
    let backend = peer(&muse);
    let first = submit(&backend, "first").await;
    let second = submit(&backend, "second").await;
    assert_ne!(first, second);
    reject(&muse, "unknown-task", "unknown task_id").await;
    assert_eq!(
        muse.report(&first.to_string()).await,
        json!({"accepted":true,"task_id":first})
    );
    reject(&muse, &first.to_string(), "task_id already reported").await;
    assert_eq!(
        backend.next_event(&first).await,
        Some(TaskEvent::Completed {
            task_id: first.clone(),
            summary: "Muse test job completed".into(),
        })
    );
    assert_eq!(backend.next_event(&first).await, None);
    let failed = muse
        .report_output(&second.to_string(), "failed", "Muse test job failed")
        .await;
    assert_eq!(failed.status.code(), Some(0));
    assert_eq!(
        serde_json::from_slice::<Value>(&failed.stdout).unwrap(),
        json!({"accepted":true,"task_id":second})
    );
    assert_eq!(
        backend.next_event(&second).await,
        Some(TaskEvent::Failed {
            task_id: second.clone(),
            retryable: false,
            message: "Muse test job failed".into(),
        })
    );
    assert_eq!(backend.next_event(&second).await, None);
    assert_eq!(muse.calls().len(), 2);
    muse.stop().await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt Muse binaries, Linux and openssl"]
async fn muse_cancel_once_joins_and_rejects_late_report() {
    let muse = Muse::start("subjectAltName=IP:127.0.0.1").await;
    let backend = peer(&muse);
    let id = submit(&backend, "cancelled").await;
    assert_eq!(backend.cancel(&id).await, Ok(()));
    assert_eq!(backend.cancel(&id).await, Ok(()));
    reject(&muse, &id.to_string(), "cancelled task_id").await;
    assert_eq!(backend.next_event(&id).await, None);
    let calls = muse.calls();
    assert_eq!(calls.len(), 2);
    let first = calls[0]["argv"].as_array().unwrap();
    let cancel = calls[1]["argv"].as_array().unwrap();
    assert_eq!(&cancel[..5], &first[..5]);
    assert_eq!(
        cancel[5],
        format!("Cancel task {id}. Do not call echo.report.result for it.")
    );
    muse.stop().await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt Muse binaries, Linux and openssl"]
async fn muse_clock_expiry_is_nonretryable_without_report() {
    let mut muse = Muse::start("subjectAltName=IP:127.0.0.1").await;
    let backend = peer(&muse);
    let id = submit(&backend, "deadline").await;
    muse.set_time(108).await;
    assert_eq!(
        backend.next_event(&id).await,
        Some(TaskEvent::Failed {
            task_id: id.clone(),
            retryable: false,
            message: "Muse did not report a result in time".into(),
        })
    );
    assert_eq!(backend.next_event(&id).await, None);
    assert_eq!(muse.calls().len(), 1);
    muse.stop().await;
}
