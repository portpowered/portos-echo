//! T1: one bridge component with explicit in-memory task and Live boundaries.
use super::*;
use echo_agent_api::AgentError;
use echo_contracts::ids::DelegationId;
use serde_json::Value;
use std::time::Duration;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio::{sync::Notify, time::Instant};
#[path = "trace_tests.rs"]
mod trace_tests;

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
fn cutoff() -> Instant {
    Instant::now() + Duration::from_millis(4_999)
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

#[derive(Default)]
struct Live(Vec<(DelegationId, String)>, bool);
impl crate::bridge_runtime::journey::bridge_live::LivePort for Live {
    fn send_audio(
        &mut self,
        _: echo_live::AudioFrame,
        _: std::time::Instant,
    ) -> impl std::future::Future<Output = Result<(), echo_live::call::CallError>> {
        std::future::ready(Err(echo_live::call::CallError::Decode))
    }
    fn receive_event(
        &mut self,
        _: std::time::Instant,
    ) -> impl std::future::Future<Output = Result<echo_contracts::live::LiveEvent, echo_live::call::CallError>> {
        std::future::ready(Err(echo_live::call::CallError::Decode))
    }
    fn append_commentary(
        &mut self,
        id: DelegationId,
        text: String,
    ) -> impl std::future::Future<Output = Result<(), echo_live::call::CallError>> {
        self.0.push((id, text));
        std::future::ready(if self.1 {
            Err(echo_live::call::CallError::Decode)
        } else {
            Ok(())
        })
    }
    fn pop_audio(&mut self) -> Option<echo_live::AudioFrame> {
        None
    }
    fn close(&mut self) {
        unreachable!()
    }
}

fn delegation(id: &str, target: &str) -> echo_contracts::live::LiveEvent {
    serde_json::from_value(
        json!({"type":"session.delegation.created","delegation":{"id":id,"target":target,"input":"private request"}}),
    )
    .unwrap()
}

#[tokio::test(start_paused = true)]
async fn bridge_muse_host_delivers_once_on_owner_and_deduplicates_busy_or_unsupported_work() {
    use super::super::bridge_muse_host::Muse;
    let fake = fake(vec![TaskEvent::Completed {
        task_id: id(),
        summary: "completed test job".into(),
    }]);
    let state = fake.state.clone();
    let mut host = Muse::new(fake, 1, Arc::new(|| 7));
    let mut witness = Witness::new();
    let event = delegation("delegation-raw-7", "client");
    assert!(host.dispatch(&event, "run-7", &mut witness.trace, cutoff()).unwrap());
    assert!(!host.dispatch(&event, "run-7", &mut witness.trace, cutoff()).unwrap());
    assert!(
        !host
            .dispatch(&delegation("busy", "client"), "run-7", &mut witness.trace, cutoff())
            .unwrap()
    );
    assert!(
        !host
            .dispatch(
                &delegation("responses", "responses"),
                "run-7",
                &mut witness.trace,
                cutoff()
            )
            .unwrap()
    );
    host.ready().await;
    let mut live = Live::default();
    host.service(&mut live, &mut witness.trace).await.unwrap();
    host.service(&mut live, &mut witness.trace).await.unwrap();
    assert_eq!(live.0, vec![(task().delegation_id, "completed test job".into())]);
    let mut expected = task();
    expected.context = serde_json::to_value(event).unwrap();
    assert_eq!(*state.tasks.lock().unwrap(), vec![expected]);
    assert!(!host.abandon().await);
    assert_eq!(*state.cancels.lock().unwrap(), vec![]);
    assert_eq!(
        witness
            .records()
            .iter()
            .filter(|v| v["event"] == "session.commentary.append")
            .count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn bridge_muse_host_abandon_joins_cancel_and_suppresses_late_commentary() {
    use super::super::bridge_muse_host::Muse;
    let fake = fake(vec![]);
    let state = fake.state.clone();
    let mut host = Muse::new(fake, 1, Arc::new(|| 7));
    let mut witness = Witness::new();
    assert!(
        !host
            .dispatch(
                &delegation("expired", "client"),
                "run-7",
                &mut witness.trace,
                Instant::now()
            )
            .unwrap()
    );
    assert!(state.tasks.lock().unwrap().is_empty());
    assert!(
        host.dispatch(
            &delegation("delegation-raw-7", "client"),
            "run-7",
            &mut witness.trace,
            cutoff()
        )
        .unwrap()
    );
    state.polled.notified().await;
    assert!(host.abandon().await);
    assert!(!host.abandon().await);
    let mut live = Live::default();
    host.service(&mut live, &mut witness.trace).await.unwrap();
    assert_eq!(live.0, vec![]);
    assert_eq!(*state.cancels.lock().unwrap(), vec![id()]);
    assert_eq!(
        witness
            .records()
            .iter()
            .map(|v| v["event"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["session.delegation.created", "muse.submit", "delegation.resolved"]
    );
}

#[tokio::test(start_paused = true)]
async fn bridge_muse_host_budget_survives_new_calls_and_cancellation_then_rolls_over_at_new_day() {
    use super::super::bridge_muse_host::Muse;
    use std::sync::atomic::{AtomicU64, Ordering};
    let fake = fake(vec![]);
    let state = fake.state.clone();
    let day = Arc::new(AtomicU64::new(7));
    let clock = day.clone();
    let mut host = Muse::new(fake, 1, Arc::new(move || clock.load(Ordering::Relaxed)));
    let mut witness = Witness::new();
    let event = delegation("delegation-raw-7", "client");
    assert!(host.dispatch(&event, "run-7", &mut witness.trace, cutoff()).unwrap());
    state.polled.notified().await;
    assert!(host.abandon().await);
    host.new_call();
    assert!(host.dispatch(&event, "run-8", &mut witness.trace, cutoff()).unwrap());
    let mut live = Live::default();
    host.service(&mut live, &mut witness.trace).await.unwrap();
    assert_eq!(
        live.0,
        vec![(task().delegation_id, "daily request limit reached".into())]
    );
    assert_eq!(state.tasks.lock().unwrap().len(), 1);
    day.store(8, Ordering::Relaxed);
    host.new_call();
    assert!(host.dispatch(&event, "run-9", &mut witness.trace, cutoff()).unwrap());
    state.polled.notified().await;
    assert!(host.abandon().await);
    assert_eq!(state.tasks.lock().unwrap().len(), 2);
    assert_eq!(*state.cancels.lock().unwrap(), vec![id(), id()]);
}
