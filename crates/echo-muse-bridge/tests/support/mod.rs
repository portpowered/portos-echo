use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use echo_agent_api::Task;
use echo_contracts::{
    ids::{DelegationId, TaskId},
    task::TaskEvent,
};
use echo_muse_bridge::{Clock, Gadget, GadgetError, Reply, SideChat};
use serde_json::{Value, json};
use tokio::sync::Notify;

#[derive(Clone, Default)]
pub struct FakeClock {
    now: Arc<AtomicU64>,
    changed: Arc<Notify>,
}

impl FakeClock {
    pub fn set(&self, now: u64) {
        self.now.store(now, Ordering::SeqCst);
        self.changed.notify_waiters();
    }
}

impl Clock for FakeClock {
    fn now(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }

    async fn wait_until(&self, deadline: u64) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.now() >= deadline {
                return;
            }
            changed.await;
        }
    }
}

#[derive(Clone, Default)]
pub struct RecordingGadget {
    pub calls: Arc<Mutex<Vec<(SideChat, String, u64)>>>,
}

impl Gadget for RecordingGadget {
    fn send_user_msg(
        &self,
        session: SideChat,
        text: &str,
        wait: u64,
    ) -> impl Future<Output = Result<Reply, GadgetError>> {
        self.calls.lock().unwrap().push((session, text.into(), wait));
        std::future::ready(Ok(Reply {
            text: "Done! The job completed successfully.".into(),
            complete: true,
        }))
    }
}

/// # Panics
/// The fixed, nonempty synthetic delegation identifier must remain valid.
#[must_use]
pub fn task() -> Task {
    Task {
        delegation_id: DelegationId::new("delegation-test").unwrap(),
        instructions: "Read the test light state.".into(),
        context: json!({"target": "test-light", "nested": ["水", 3, true]}),
        deadline_seconds: 60,
        max_tool_calls: 8,
    }
}

#[must_use]
pub fn report(id: &TaskId, status: &str) -> String {
    json!({"task_id": id, "status": status, "summary": "light is on"}).to_string()
}

#[must_use]
pub fn progress(id: &TaskId) -> TaskEvent {
    TaskEvent::Progress {
        task_id: id.clone(),
        message: "Done! The job completed successfully.".into(),
    }
}

#[must_use]
pub fn completed(id: &TaskId) -> TaskEvent {
    TaskEvent::Completed {
        task_id: id.clone(),
        summary: "light is on".into(),
    }
}

#[must_use]
pub fn timeout(id: &TaskId) -> TaskEvent {
    TaskEvent::Failed {
        task_id: id.clone(),
        retryable: false,
        message: "Muse did not report a result in time".into(),
    }
}

#[must_use]
pub fn accepted(id: &TaskId) -> Value {
    json!({"accepted": true, "task_id": id})
}
