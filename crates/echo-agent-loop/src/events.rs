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
        let mut state = lock(&self.state);
        if state.terminal {
            worker.abort();
        } else {
            state.worker = Some(worker);
        }
    }

    pub(crate) async fn drive(
        &self,
        run: impl std::future::Future<Output = Result<String, &'static str>>,
        deadline: tokio::time::Instant,
    ) -> Result<String, &'static str> {
        tokio::pin!(run);
        let timer = tokio::time::sleep_until(deadline);
        tokio::pin!(timer);
        std::future::poll_fn(|cx| {
            // Serialize each poll with cancellation: once terminal is committed,
            // even a ready continuation cannot start another side effect.
            let state = lock(&self.state);
            if state.terminal {
                return std::task::Poll::Ready(Err("task cancelled"));
            }
            if timer.as_mut().poll(cx).is_ready() {
                return std::task::Poll::Ready(Err("task timed out"));
            }
            run.as_mut().poll(cx)
        })
        .await
    }

    pub(crate) fn finish(&self, id: &TaskId, result: Result<String, &'static str>) {
        let mut state = lock(&self.state);
        if state.terminal {
            return;
        }
        state.terminal = true;
        state.worker.take();
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

    pub(crate) async fn cancel(&self, id: &TaskId) {
        let worker = lock(&self.state).worker.take();
        self.finish(id, Err("task cancelled"));
        if let Some(worker) = worker {
            worker.abort();
            let _ = worker.await;
        }
    }

    pub(crate) fn stop(&self) {
        if let Some(worker) = lock(&self.state).worker.take() {
            worker.abort();
        }
    }
}
#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;
