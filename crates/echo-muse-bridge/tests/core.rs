//! T1 component flows through the public backend and intake, with no real time.
#[path = "cases/races.rs"]
mod races;
#[path = "cases/report.rs"]
mod report_cases;
#[path = "cases/store.rs"]
mod store_cases;
#[path = "cases/store_faults.rs"]
mod store_faults;
mod support;

use echo_agent_api::{AgentBackend, AgentError};
use echo_contracts::{ids::TaskId, task::TaskEvent};
use echo_muse_bridge::{Gadget, GadgetError, InMemoryStore, MuseBackend, Reply, SideChat, compose_task_message};
use support::{FakeClock, RecordingGadget, accepted, completed, progress, report, task, timeout};

#[tokio::test]
async fn submit_isolates_chats_and_only_correlated_report_completes() {
    let gadget = RecordingGadget::default();
    let backend = MuseBackend::new(gadget.clone(), FakeClock::default(), InMemoryStore::default());
    let first = backend.submit(task()).await.unwrap();
    let second = backend.submit(task()).await.unwrap();
    assert_ne!(first, second);
    let calls = gadget.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert_ne!(calls[0].0, calls[1].0);
    for (call, id) in calls.iter().zip([&first, &second]) {
        assert_eq!(uuid::Uuid::parse_str(&call.0.to_string()).unwrap().get_version_num(), 4);
        assert_eq!(call.2, 1);
        assert_eq!(
            call.1,
            format!(
                "Read the test light state.\nContext: {{\"nested\":[\"水\",3,true],\"target\":\"test-light\"}}\nTask ID: {id}\nWhen you are finished, call the echo.report.result command with task_id {id}, status completed or failed, and a short summary. Do not report success any other way."
            )
        );
        assert_eq!(call.1, compose_task_message(&task(), id));
    }
    assert_eq!(backend.next_event(&first).await, Some(progress(&first)));
    assert_eq!(
        backend.report_intake().accept(&report(&first, "completed")).unwrap(),
        accepted(&first)
    );
    assert_eq!(
        backend
            .report_intake()
            .accept(&report(&first, "completed"))
            .unwrap_err()
            .to_string(),
        "task_id already reported"
    );
    assert_eq!(backend.next_event(&first).await, Some(completed(&first)));
    assert_eq!(backend.next_event(&first).await, None);
    assert_eq!(
        backend
            .report_intake()
            .accept(&report(&first, "failed"))
            .unwrap_err()
            .to_string(),
        "task_id already reported"
    );
    assert_eq!(backend.next_event(&second).await, Some(progress(&second)));
}

#[tokio::test]
async fn failed_reports_are_nonretryable_and_unknown_ids_are_isolated() {
    let backend = MuseBackend::new(
        RecordingGadget::default(),
        FakeClock::default(),
        InMemoryStore::default(),
    );
    let id = backend.submit(task()).await.unwrap();
    let unknown = TaskId::new("unknown").unwrap();
    assert_eq!(backend.cancel(&unknown).await, Err(AgentError::UnknownTask));
    assert_eq!(backend.next_event(&unknown).await, None);
    assert_eq!(
        backend
            .report_intake()
            .accept(&report(&unknown, "completed"))
            .unwrap_err()
            .to_string(),
        "unknown task_id"
    );
    assert_eq!(
        backend.report_intake().accept(&report(&id, "failed")).unwrap(),
        accepted(&id)
    );
    assert_eq!(backend.next_event(&id).await, Some(progress(&id)));
    assert_eq!(
        backend.next_event(&id).await,
        Some(TaskEvent::Failed {
            task_id: id.clone(),
            retryable: false,
            message: "light is on".into(),
        })
    );
    assert_eq!(backend.next_event(&id).await, None);
}

#[tokio::test]
async fn pending_wait_observes_deadline_equality_once_and_isolates_tasks() {
    let clock = FakeClock::default();
    clock.set(100);
    let backend = MuseBackend::new(RecordingGadget::default(), clock.clone(), InMemoryStore::default());
    let id = backend.submit(task()).await.unwrap();
    let other = backend
        .submit(echo_agent_api::Task {
            deadline_seconds: 90,
            ..task()
        })
        .await
        .unwrap();
    assert_eq!(backend.next_event(&id).await, Some(progress(&id)));
    clock.set(159);
    let waiting = backend.next_event(&id);
    tokio::pin!(waiting);
    assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx).is_pending())).await);
    clock.set(160);
    assert_eq!(waiting.await, Some(timeout(&id)));
    assert_eq!(backend.next_event(&id).await, None);
    assert_eq!(
        backend
            .report_intake()
            .accept(&report(&id, "completed"))
            .unwrap_err()
            .to_string(),
        "expired task_id"
    );
    assert_eq!(
        backend.report_intake().accept(&report(&other, "completed")).unwrap(),
        accepted(&other)
    );
    assert_eq!(backend.next_event(&other).await, Some(progress(&other)));
    assert_eq!(backend.next_event(&other).await, Some(completed(&other)));
}

#[tokio::test]
async fn cancel_discards_queue_uses_original_chat_and_wakes_waiter() {
    let gadget = RecordingGadget::default();
    let backend = MuseBackend::new(gadget.clone(), FakeClock::default(), InMemoryStore::default());
    let id = backend.submit(task()).await.unwrap();
    backend.cancel(&id).await.unwrap();
    assert_eq!(backend.next_event(&id).await, None);
    assert_eq!(
        backend
            .report_intake()
            .accept(&report(&id, "completed"))
            .unwrap_err()
            .to_string(),
        "cancelled task_id"
    );
    let calls = gadget.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[1],
        (
            calls[0].0,
            format!("Cancel task {id}. Do not call echo.report.result for it."),
            1
        )
    );
    let second = backend.submit(task()).await.unwrap();
    assert_eq!(backend.next_event(&second).await, Some(progress(&second)));
    let waiting = backend.next_event(&second);
    tokio::pin!(waiting);
    assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx).is_pending())).await);
    backend.cancel(&second).await.unwrap();
    assert_eq!(waiting.await, None);
}

struct PartialGadget;

impl Gadget for PartialGadget {
    fn send_user_msg(&self, _: SideChat, _: &str, _: u64) -> impl Future<Output = Result<Reply, GadgetError>> {
        std::future::ready(Ok(Reply {
            text: "Still working".into(),
            complete: false,
        }))
    }
}

#[tokio::test]
async fn incomplete_reply_is_progress_and_report_wakes_pending_consumer() {
    let backend = MuseBackend::new(PartialGadget, FakeClock::default(), InMemoryStore::default());
    let id = backend.submit(task()).await.unwrap();
    assert_eq!(
        backend.next_event(&id).await,
        Some(TaskEvent::Progress {
            task_id: id.clone(),
            message: "Still working".into()
        })
    );
    let waiting = backend.next_event(&id);
    tokio::pin!(waiting);
    assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx).is_pending())).await);
    assert_eq!(
        backend.report_intake().accept(&report(&id, "completed")).unwrap(),
        accepted(&id)
    );
    assert_eq!(waiting.await, Some(completed(&id)));
}

struct FailingGadget;

impl Gadget for FailingGadget {
    fn send_user_msg(&self, _: SideChat, _: &str, _: u64) -> impl Future<Output = Result<Reply, GadgetError>> {
        std::future::ready(Err(GadgetError::TrustStore))
    }
}

#[tokio::test]
async fn send_failure_is_safe_and_failed_cancel_remains_cancelled() {
    let backend = MuseBackend::new(FailingGadget, FakeClock::default(), InMemoryStore::default());
    let id = backend.submit(task()).await.unwrap();
    assert_eq!(
        backend.next_event(&id).await,
        Some(TaskEvent::Failed {
            task_id: id.clone(),
            retryable: false,
            message: "Muse gadget request failed".into(),
        })
    );
    assert_eq!(backend.next_event(&id).await, None);
    backend.cancel(&id).await.unwrap();
    assert_eq!(backend.next_event(&id).await, None);
    assert_eq!(
        backend
            .report_intake()
            .accept(&report(&id, "completed"))
            .unwrap_err()
            .to_string(),
        "cancelled task_id"
    );
}

#[tokio::test]
async fn overflow_refuses_admission_before_send() {
    let gadget = RecordingGadget::default();
    let clock = FakeClock::default();
    clock.set(u64::MAX);
    let backend = MuseBackend::new(gadget.clone(), clock, InMemoryStore::default());
    assert_eq!(backend.submit(task()).await, Err(AgentError::UnscriptedTask));
    assert_eq!(*gadget.calls.lock().unwrap(), vec![]);
}
