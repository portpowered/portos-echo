#![cfg(test)]
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use echo_agent_api::AgentBackend;
use echo_contracts::ids::TaskId;
use echo_muse_bridge::{Gadget, GadgetError, InMemoryStore, MuseBackend, Reply, SideChat};
use tokio::sync::Semaphore;

use crate::support::{FakeClock, RecordingGadget, accepted, completed, progress, report, task, timeout};

struct HeldGadget {
    calls: Mutex<Vec<(SideChat, String)>>,
    release: Semaphore,
    active: AtomicUsize,
}

impl HeldGadget {
    fn new() -> Self {
        Self {
            calls: Mutex::new(vec![]),
            release: Semaphore::new(0),
            active: AtomicUsize::new(0),
        }
    }

    fn admitted(&self) -> TaskId {
        let calls = self.calls.lock().unwrap();
        let id = calls[0]
            .1
            .lines()
            .find_map(|line| line.strip_prefix("Task ID: "))
            .unwrap();
        TaskId::new(id).unwrap()
    }
}

struct HeldHandle(Arc<HeldGadget>);

impl Gadget for HeldHandle {
    async fn send_user_msg(&self, session: SideChat, text: &str, _: u64) -> Result<Reply, GadgetError> {
        assert_eq!(
            self.0.active.fetch_add(1, Ordering::SeqCst),
            0,
            "no overlapping waits in the same chat"
        );
        self.0.calls.lock().unwrap().push((session, text.into()));
        if !text.starts_with("Cancel task ") {
            self.0.release.acquire().await.unwrap().forget();
        }
        self.0.active.fetch_sub(1, Ordering::SeqCst);
        Ok(Reply {
            text: "late chat text".into(),
            complete: true,
        })
    }
}

#[tokio::test]
async fn report_during_send_suppresses_late_progress() {
    let gadget = Arc::new(HeldGadget::new());
    let backend = MuseBackend::new(
        HeldHandle(gadget.clone()),
        FakeClock::default(),
        InMemoryStore::default(),
    );
    let submit = backend.submit(task());
    tokio::pin!(submit);
    assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(submit.as_mut().poll(cx).is_pending())).await);
    let id = gadget.admitted();
    assert_eq!(
        backend.report_intake().accept(&report(&id, "completed")).unwrap(),
        accepted(&id)
    );
    gadget.release.add_permits(1);
    assert_eq!(submit.await.unwrap(), id);
    assert_eq!(backend.next_event(&id).await, Some(completed(&id)));
    assert_eq!(backend.next_event(&id).await, None);
}

#[tokio::test]
async fn cancellation_commits_while_send_pending_and_serializes_advisory() {
    let gadget = Arc::new(HeldGadget::new());
    let backend = MuseBackend::new(
        HeldHandle(gadget.clone()),
        FakeClock::default(),
        InMemoryStore::default(),
    );
    let submit = backend.submit(task());
    tokio::pin!(submit);
    assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(submit.as_mut().poll(cx).is_pending())).await);
    let id = gadget.admitted();
    let cancel = backend.cancel(&id);
    tokio::pin!(cancel);
    assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(cancel.as_mut().poll(cx).is_pending())).await);
    assert_eq!(gadget.calls.lock().unwrap().len(), 1);
    assert_eq!(
        backend
            .report_intake()
            .accept(&report(&id, "completed"))
            .unwrap_err()
            .to_string(),
        "cancelled task_id"
    );
    assert_eq!(backend.next_event(&id).await, None);
    gadget.release.add_permits(1);
    assert_eq!(submit.await.unwrap(), id);
    cancel.await.unwrap();
    assert_eq!(backend.next_event(&id).await, None);
    let calls = gadget.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[1],
        (
            calls[0].0,
            format!("Cancel task {id}. Do not call echo.report.result for it.")
        )
    );
}

#[tokio::test]
async fn report_timeout_orders_have_one_terminal_and_cancel_wins_final_state() {
    for report_first in [true, false] {
        let clock = FakeClock::default();
        let backend = MuseBackend::new(RecordingGadget::default(), clock.clone(), InMemoryStore::default());
        let id = backend.submit(task()).await.unwrap();
        assert_eq!(backend.next_event(&id).await, Some(progress(&id)));
        if report_first {
            clock.set(59);
            assert_eq!(
                backend.report_intake().accept(&report(&id, "completed")).unwrap(),
                accepted(&id)
            );
            clock.set(60);
            assert_eq!(backend.next_event(&id).await, Some(completed(&id)));
        } else {
            clock.set(60);
            assert_eq!(
                backend
                    .report_intake()
                    .accept(&report(&id, "completed"))
                    .unwrap_err()
                    .to_string(),
                "expired task_id"
            );
            assert_eq!(backend.next_event(&id).await, Some(timeout(&id)));
        }
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
}

#[tokio::test]
async fn accepted_then_cancelled_clears_queued_terminal_and_repeated_cancel_is_safe() {
    let backend = MuseBackend::new(
        RecordingGadget::default(),
        FakeClock::default(),
        InMemoryStore::default(),
    );
    let id = backend.submit(task()).await.unwrap();
    backend.report_intake().accept(&report(&id, "completed")).unwrap();
    backend.cancel(&id).await.unwrap();
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
