use std::{collections::VecDeque, future::pending, time::Duration};

use echo_contracts::ids::TaskId;
use futures_util::{FutureExt as _, StreamExt as _, future::BoxFuture, stream::FuturesUnordered};
use serde_json::{Value, json};

use crate::{
    ServiceError,
    tasks::{Report, Tasks},
};

use super::{REPORT, REPORT_TIMEOUT_MS};

const ACTIVE_LIMIT: usize = 4;
const WAITING_LIMIT: usize = 16;

struct Invocation {
    id: String,
    params: Report,
    timeout: Option<Duration>,
}

pub(crate) struct Reply {
    pub(crate) message: Value,
    pub(crate) task: Option<TaskId>,
}

impl Reply {
    fn refused(id: &str, error: &str) -> Self {
        Self {
            message: json!({"method":"link.result", "id":id, "ok":false, "error":error}),
            task: None,
        }
    }

    fn accepted(id: &str, payload: &Value, task: Option<TaskId>) -> Self {
        Self {
            message: json!({"method":"link.result", "id":id, "ok":true, "payload":payload}),
            task,
        }
    }
}

pub(crate) struct Handler {
    report_timeout: Duration,
    #[cfg(test)]
    held: Option<tokio::sync::mpsc::Sender<HeldInvoke>>,
}

impl Default for Handler {
    fn default() -> Self {
        Self {
            report_timeout: Duration::from_millis(REPORT_TIMEOUT_MS),
            #[cfg(test)]
            held: None,
        }
    }
}

#[cfg(test)]
pub(crate) struct HeldInvoke {
    pub(crate) id: String,
    pub(crate) finish: tokio::sync::oneshot::Sender<Option<String>>,
}

impl Handler {
    #[cfg(test)]
    pub(crate) fn holding(held: tokio::sync::mpsc::Sender<HeldInvoke>) -> Self {
        Self {
            held: Some(held),
            ..Self::default()
        }
    }

    async fn execute(&self, tasks: &Tasks, invoke: Invocation) -> Reply {
        let id = invoke.id;
        let timeout = invoke.timeout.unwrap_or(self.report_timeout).min(self.report_timeout);
        let dispatch = async {
            #[cfg(test)]
            if let Some(held) = &self.held {
                let (finish, output) = tokio::sync::oneshot::channel();
                if held.send(HeldInvoke { id: id.clone(), finish }).await.is_err() {
                    return Reply::refused(&id, "command failed");
                }
                match output.await {
                    Ok(Some(output)) => return Reply::accepted(&id, &json!({"output":output}), None),
                    Ok(None) => {}
                    Err(_) => return Reply::refused(&id, "command failed"),
                }
            }
            report(tasks, &id, invoke.params)
        };
        match tokio::time::timeout(timeout, dispatch).await {
            Ok(reply) => reply,
            Err(_) => Reply::refused(&id, "invoke timed out"),
        }
    }
}

fn report(tasks: &Tasks, id: &str, params: Report) -> Reply {
    let task = params.task_id.clone();
    match tasks.report(params) {
        Ok(()) => Reply::accepted(id, &json!({"accepted":true,"task_id":task}), Some(task)),
        Err(ServiceError::UnknownTask) => Reply::refused(id, "unknown task_id"),
        Err(_) => Reply::refused(id, "task storage failed"),
    }
}

// The link owns and polls these futures. Dropping the link drops all active
// handlers and queued requests without leaving detached invocation work.
pub(crate) struct Invocations<'a> {
    handler: &'a Handler,
    tasks: &'a Tasks,
    active: FuturesUnordered<BoxFuture<'a, Reply>>,
    waiting: VecDeque<Invocation>,
}

impl<'a> Invocations<'a> {
    pub(crate) fn new(handler: &'a Handler, tasks: &'a Tasks) -> Self {
        Self {
            handler,
            tasks,
            active: FuturesUnordered::new(),
            waiting: VecDeque::new(),
        }
    }

    pub(crate) fn admit(&mut self, mut message: Value) -> Option<Reply> {
        if message["method"] != "link.invoke" {
            return None;
        }
        let id = message["id"].as_str().filter(|id| !id.is_empty())?.to_owned();
        if message["command"] != REPORT {
            return Some(Reply::refused(&id, "unknown command"));
        }
        let Ok(params) = serde_json::from_value(message["params"].take()) else {
            return Some(Reply::refused(&id, "invalid report parameters"));
        };
        let timeout = message["timeout_ms"]
            .as_u64()
            .filter(|&value| value != 0)
            .map(Duration::from_millis);
        let invoke = Invocation { id, params, timeout };
        if self.active.len() < ACTIVE_LIMIT {
            self.start(invoke);
        } else if self.waiting.len() < WAITING_LIMIT {
            self.waiting.push_back(invoke);
        } else {
            return Some(Reply::refused(&invoke.id, "invoke queue is full"));
        }
        None
    }

    pub(crate) async fn next(&mut self) -> Reply {
        let Some(reply) = self.active.next().await else {
            return pending().await;
        };
        if let Some(invoke) = self.waiting.pop_front() {
            self.start(invoke);
        }
        reply
    }

    fn start(&mut self, invoke: Invocation) {
        self.active.push(self.handler.execute(self.tasks, invoke).boxed());
    }
}
