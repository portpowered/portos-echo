use crate::lock;
use echo_contracts::{ids::TaskId, task::TaskEvent};
use std::{collections::VecDeque, sync::Mutex};
use tokio::{sync::Notify, task::JoinHandle};

struct State {
    queue: VecDeque<TaskEvent>,
    terminal: bool,
    worker: Option<JoinHandle<()>>,
}

pub(crate) struct Events {
    state: Mutex<State>,
    notify: Notify,
}

impl Events {
    pub(crate) fn new(id: &TaskId) -> Self {
        Self {
            state: Mutex::new(State {
                queue: VecDeque::from([TaskEvent::Progress {
                    task_id: id.clone(),
                    message: "task started".into(),
                }]),
                terminal: false,
                worker: None,
            }),
            notify: Notify::new(),
        }
    }

    pub(crate) fn set_worker(&self, worker: JoinHandle<()>) {
        lock(&self.state).worker = Some(worker);
    }

    pub(crate) fn finish(&self, id: &TaskId, result: Result<String, &'static str>) {
        let mut state = lock(&self.state);
        if state.terminal {
            return;
        }
        state.terminal = true;
        state.queue.push_back(match result {
            Ok(summary) => TaskEvent::Completed {
                task_id: id.clone(),
                summary,
            },
            Err(message) => TaskEvent::Failed {
                task_id: id.clone(),
                retryable: false,
                message: message.into(),
            },
        });
        self.notify.notify_waiters();
    }

    pub(crate) async fn next(&self) -> Option<TaskEvent> {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = lock(&self.state);
                if let Some(event) = state.queue.pop_front() {
                    return Some(event);
                }
                if state.terminal {
                    return None;
                }
            }
            notified.await;
        }
    }

    pub(crate) fn cancel(&self, id: &TaskId) {
        self.stop();
        self.finish(id, Err("task cancelled"));
    }

    pub(crate) fn stop(&self) {
        if let Some(worker) = lock(&self.state).worker.take() {
            worker.abort();
        }
    }
}
