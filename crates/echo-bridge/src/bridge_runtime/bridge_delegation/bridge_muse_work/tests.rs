//! T1: one bridge component with an in-memory task backend.
use super::*;
use echo_agent_api::AgentError;
use echo_contracts::ids::DelegationId;
use serde_json::Value;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio::sync::Notify;

#[derive(Default)]
struct State {
    tasks: Mutex<Vec<Task>>,
    polls: Mutex<Vec<TaskId>>,
    cancels: Mutex<Vec<TaskId>>,
    events: Mutex<VecDeque<TaskEvent>>,
    entered: Notify,
    admitted: Notify,
    polled: Notify,
    release: Notify,
}

#[derive(Clone)]
struct Fake {
    state: Arc<State>,
    reject: bool,
    gate: bool,
}
impl AgentBackend for Fake {
    async fn submit(&self, task: Task) -> Result<TaskId, AgentError> {
        self.state.tasks.lock().unwrap().push(task);
        self.state.entered.notify_one();
        if self.gate {
            self.state.release.notified().await;
        }
        self.state.admitted.notify_one();
        if self.reject {
            Err(AgentError::UnscriptedTask)
        } else {
            Ok(id())
        }
    }
    async fn next_event(&self, id: &TaskId) -> Option<TaskEvent> {
        self.state.polls.lock().unwrap().push(id.clone());
        self.state.polled.notify_one();
        let event = self.state.events.lock().unwrap().pop_front();
        match event {
            Some(event) => Some(event),
            None => std::future::pending().await,
        }
    }
    fn cancel(&self, id: &TaskId) -> impl std::future::Future<Output = Result<(), AgentError>> {
        self.state.cancels.lock().unwrap().push(id.clone());
        std::future::ready(Ok(()))
    }
}
fn id() -> TaskId {
    TaskId::new("task-raw-7").unwrap()
}
fn task() -> Task {
    Task {
        delegation_id: DelegationId::new("delegation-raw-7").unwrap(),
        instructions: "Execute the delegated request and report its result.".into(),
        context: json!({"input":"private request"}),
        deadline_seconds: 5,
        max_tool_calls: 8,
    }
}
fn fake(events: Vec<TaskEvent>) -> Fake {
    let state = Arc::new(State::default());
    *state.events.lock().unwrap() = events.into();
    Fake {
        state,
        reject: false,
        gate: false,
    }
}
struct Witness {
    _directory: tempfile::TempDir,
    path: std::path::PathBuf,
    trace: Trace,
}
impl Witness {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        Self {
            trace: Trace::open(&path, "api_key").unwrap(),
            path,
            _directory: directory,
        }
    }
    fn records(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.path)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }
}
fn fields(event: &str) -> Value {
    json!({"event":event,"run_id":"run-7","delegation_id":"delegation-raw-7","task_id":"task-raw-7",
        "provider_auth_mode":"api_key","elapsed_ms":0})
}

#[tokio::test(start_paused = true)]
async fn muse_work_progress_then_one_terminal_keeps_raw_ids_and_private_text_out_of_trace() {
    let fake = fake(vec![
        TaskEvent::Progress {
            task_id: id(),
            message: "private gadget chat".into(),
        },
        TaskEvent::Completed {
            task_id: id(),
            summary: "completed test job".into(),
        },
        TaskEvent::Completed {
            task_id: id(),
            summary: "duplicate".into(),
        },
    ]);
    let witness = Witness::new();
    let (_stop, receive) = oneshot::channel();
    assert_eq!(
        execute(&fake, task(), "run-7", witness.trace.clone(), receive)
            .await
            .map(|value| value.map(|prepared| prepared.text)),
        Ok(Some("completed test job".into()))
    );
    assert_eq!(*fake.state.tasks.lock().unwrap(), vec![task()]);
    assert_eq!(*fake.state.polls.lock().unwrap(), vec![id(), id()]);
    assert_eq!(*fake.state.cancels.lock().unwrap(), vec![]);
    let mut terminal = fields("muse.terminal");
    terminal["status"] = "completed".into();
    assert_eq!(
        witness.records(),
        vec![fields("muse.submit"), fields("muse.progress"), terminal]
    );
}

#[tokio::test(start_paused = true)]
async fn muse_work_rejected_admission_has_no_remote_id_or_fabricated_terminal() {
    let mut fake = fake(vec![]);
    fake.reject = true;
    let witness = Witness::new();
    let (_stop, receive) = oneshot::channel();
    assert_eq!(
        execute(&fake, task(), "run-7", witness.trace.clone(), receive)
            .await
            .map(|value| value.map(|prepared| prepared.text)),
        Ok(Some(UNAVAILABLE.into()))
    );
    assert_eq!(*fake.state.tasks.lock().unwrap(), vec![task()]);
    assert_eq!(*fake.state.polls.lock().unwrap(), vec![]);
    assert_eq!(*fake.state.cancels.lock().unwrap(), vec![]);
    assert_eq!(witness.records(), Vec::<Value>::new());
}

#[tokio::test(start_paused = true)]
async fn bridge_muse_waits_for_owner_after_the_legacy_deadline() {
    let fake = fake(vec![]);
    let state = fake.state.clone();
    let witness = Witness::new();
    let trace = witness.trace.clone();
    let (stop, receive) = oneshot::channel();
    let worker = tokio::spawn(async move { execute(&fake, task(), "run-7", trace, receive).await });
    state.polled.notified().await;
    tokio::time::advance(Duration::from_secs(61)).await;
    assert!(!worker.is_finished());
    stop.send(()).unwrap();
    assert_eq!(
        worker.await.unwrap().map(|value| value.map(|prepared| prepared.text)),
        Ok(None)
    );
    assert_eq!(*state.cancels.lock().unwrap(), vec![id()]);
    assert_eq!(witness.records(), vec![fields("muse.submit")]);
}

#[tokio::test(start_paused = true)]
async fn muse_work_cancel_racing_admission_retains_the_id_then_joins_one_cancel_without_polling() {
    let mut fake = fake(vec![]);
    fake.gate = true;
    let state = fake.state.clone();
    let witness = Witness::new();
    let trace = witness.trace.clone();
    let (stop, receive) = oneshot::channel();
    let worker = tokio::spawn(async move { execute(&fake, task(), "run-7", trace, receive).await });
    state.entered.notified().await;
    stop.send(()).unwrap();
    state.release.notify_one();
    assert_eq!(
        worker.await.unwrap().map(|value| value.map(|prepared| prepared.text)),
        Ok(None)
    );
    assert_eq!(*state.tasks.lock().unwrap(), vec![task()]);
    assert_eq!(*state.polls.lock().unwrap(), vec![]);
    assert_eq!(*state.cancels.lock().unwrap(), vec![id()]);
    assert_eq!(witness.records(), vec![fields("muse.submit")]);
}

#[tokio::test(start_paused = true)]
async fn muse_work_cancel_drops_pending_poll_then_joins_cancel_without_terminal() {
    let fake = fake(vec![]);
    let state = fake.state.clone();
    let witness = Witness::new();
    let trace = witness.trace.clone();
    let (stop, receive) = oneshot::channel();
    let worker = tokio::spawn(async move { execute(&fake, task(), "run-7", trace, receive).await });
    state.polled.notified().await;
    stop.send(()).unwrap();
    assert_eq!(
        worker.await.unwrap().map(|value| value.map(|prepared| prepared.text)),
        Ok(None)
    );
    assert_eq!(*state.polls.lock().unwrap(), vec![id()]);
    assert_eq!(*state.cancels.lock().unwrap(), vec![id()]);
    assert_eq!(witness.records(), vec![fields("muse.submit")]);
}

#[tokio::test(start_paused = true)]
async fn muse_work_failure_sanitizes_untrusted_message_and_forces_nonretryable_trace() {
    for (message, expected) in [
        ("private provider body", UNAVAILABLE),
        ("Muse test job failed", "Muse test job failed"),
    ] {
        let fake = fake(vec![TaskEvent::Failed {
            task_id: id(),
            retryable: true,
            message: message.into(),
        }]);
        let witness = Witness::new();
        let (_stop, receive) = oneshot::channel();
        assert_eq!(
            execute(&fake, task(), "run-7", witness.trace.clone(), receive)
                .await
                .map(|value| value.map(|prepared| prepared.text)),
            Ok(Some(expected.into()))
        );
        let mut terminal = fields("muse.terminal");
        terminal["status"] = "failed".into();
        terminal["retryable"] = false.into();
        assert_eq!(witness.records(), vec![fields("muse.submit"), terminal]);
    }
}
