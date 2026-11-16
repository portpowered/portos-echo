//! Own and join Muse work, including a cancellation racing admission.
use super::{MAX_DELEGATIONS, bridge_muse_work};
use crate::bridge_runtime::{bridge_budget::Budget, journey::bridge_live::LivePort};
use crate::{Error, bridge_trace::Trace};
use echo_agent_api::Task;
use echo_config::AgentConfig;
use echo_contracts::{
    ids::DelegationId,
    live::{DelegationTarget, LiveEvent},
};
use serde_json::json;
use std::{collections::HashSet, sync::Arc};
use tokio::{sync::oneshot, task::JoinHandle};

type Output = Result<Option<String>, Error>;

pub(in crate::bridge_runtime) struct Muse<B = crate::bridge_muse::Backend> {
    backend: Arc<B>,
    config: AgentConfig,
    budget: Budget,
    utc_day: Arc<dyn Fn() -> u64 + Send + Sync>,
    seen: HashSet<String>,
    active: Option<(String, DelegationId)>,
    stop: Option<oneshot::Sender<()>>,
    pending: Option<JoinHandle<Output>>,
    completed: Option<Output>,
}

impl<B: echo_agent_api::AgentBackend + Send + Sync + 'static> Muse<B> {
    pub fn new(backend: B, config: AgentConfig, utc_day: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        Self {
            backend: Arc::new(backend),
            budget: Budget::new(config.daily_request_cap),
            config,
            utc_day,
            seen: HashSet::new(),
            active: None,
            stop: None,
            pending: None,
            completed: None,
        }
    }

    pub fn new_call(&mut self) {
        self.seen.clear();
    }

    pub fn dispatch(&mut self, event: &LiveEvent, run: &str, trace: &mut Trace) -> Result<bool, Error> {
        let LiveEvent::DelegationCreated { delegation, .. } = event else {
            return Ok(false);
        };
        let id = delegation.id.to_string();
        if id.len() > 256 || id.chars().any(char::is_control) {
            return Err(Error::Decode);
        }
        if self.seen.contains(&id) {
            return Ok(false);
        }
        if self.seen.len() >= MAX_DELEGATIONS as usize {
            return Err(Error::Unavailable);
        }
        self.seen.insert(id.clone());
        if delegation.target != DelegationTarget::Client || self.active.is_some() {
            trace.record(
                "delegation.refused",
                Some(run),
                json!({"delegation_id":id,"reason":"target_or_busy"}),
            )?;
            return Ok(false);
        }
        let task = Task {
            delegation_id: delegation.id.clone(),
            instructions: "Execute the delegated request and report its result.".into(),
            context: serde_json::to_value(event).map_err(|_| Error::Decode)?,
            deadline_seconds: self.config.task_deadline_seconds,
            max_tool_calls: self.config.max_tool_calls,
        };
        trace.record("session.delegation.created", Some(run), json!({"delegation_id":id}))?;
        self.active = Some((run.into(), task.delegation_id.clone()));
        if self.budget.reserve((self.utc_day)()).is_err() {
            self.completed = Some(Ok(Some("daily request limit reached".into())));
            return Ok(true);
        }
        let (stop, receive) = oneshot::channel();
        self.stop = Some(stop);
        let backend = self.backend.clone();
        let trace = trace.clone();
        let run = run.to_owned();
        self.pending = Some(tokio::spawn(async move {
            bridge_muse_work::execute(&*backend, task, &run, trace, receive).await
        }));
        Ok(true)
    }

    pub async fn ready(&mut self) {
        let Some(pending) = self.pending.as_mut() else {
            return std::future::pending().await;
        };
        self.completed = Some(pending.await.map_err(|_| Error::Unavailable).and_then(|value| value));
        self.pending = None;
    }

    pub async fn service(&mut self, call: &mut impl LivePort, trace: &mut Trace) -> Result<(), Error> {
        if self.pending.as_ref().is_some_and(JoinHandle::is_finished) {
            self.ready().await;
        }
        let Some(output) = self.completed.take() else {
            return Ok(());
        };
        self.stop = None;
        let (run, id) = self.active.take().ok_or(Error::Unavailable)?;
        if let Some(text) = output? {
            call.append_commentary(id.clone(), text).await?;
            trace.record("session.commentary.append", Some(&run), json!({"delegation_id":id}))?;
        }
        Ok(())
    }

    pub async fn abandon(&mut self) -> bool {
        let abandoned = self.active.take().is_some();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(pending) = self.pending.take() {
            let _ = pending.await;
        }
        self.completed = None;
        abandoned
    }
}

impl<B> Drop for Muse<B> {
    fn drop(&mut self) {
        // Emergency task drop only; normal shutdown explicitly signals cancel and joins.
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(pending) = self.pending.take() {
            pending.abort();
        }
    }
}
