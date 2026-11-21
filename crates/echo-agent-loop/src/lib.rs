//! Bridge-side delegated Responses backend. Wire compatibility is PROVISIONAL until M2.
mod budget;
mod conversation;
mod events;
mod transport;
mod wire;

use core::future::Future;
use echo_agent_api::{AgentBackend, AgentError, Task, ToolHost};
use echo_config::{AgentConfig, LiveMode};
use echo_contracts::{ids::TaskId, secret::Secret, task::TaskEvent};
use reqwest::header::HeaderMap;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Debug, thiserror::Error)]
#[error("model credential unavailable")]
pub struct CredentialError;

/// Fetch fresh local credentials for each POST; never return a printable secret.
pub trait ModelCredential {
    fn headers(&self, mode: LiveMode) -> impl Future<Output = Result<Secret<HeaderMap>, CredentialError>> + Send + '_;
}

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("agent.model: explicit nonblank model required")]
    Model,
    #[error("model client unavailable")]
    Client,
    #[error("test endpoint must be numeric 127.0.0.1 HTTP")]
    Endpoint,
}

/// One backend owns its client and task workers. Dropping it stops those workers.
pub struct ResponsesBackend<H, C> {
    host: Arc<H>,
    credential: Arc<C>,
    transport: transport::Transport,
    config: AgentConfig,
    budget: Arc<budget::Budget>,
    tasks: Mutex<HashMap<TaskId, Option<Arc<events::Events>>>>,
    sequence: AtomicU64,
}

impl<H, C> ResponsesBackend<H, C> {
    /// Construct without fetching credentials or opening connections.
    /// # Errors
    /// Rejects an absent/blank model or HTTP client initialization failure.
    pub fn new(config: AgentConfig, mode: LiveMode, host: Arc<H>, credential: Arc<C>) -> Result<Self, InitError> {
        Self::build(config, mode, host, credential, None)
    }

    /// T3 seam: retains the production path and permits only numeric loopback HTTP.
    /// # Errors
    /// Returns the initialization error or rejects a non-loopback base URL.
    pub fn loopback(
        config: AgentConfig,
        mode: LiveMode,
        host: Arc<H>,
        credential: Arc<C>,
        base: &str,
    ) -> Result<Self, InitError> {
        Self::build(config, mode, host, credential, Some(base))
    }

    fn build(
        config: AgentConfig,
        mode: LiveMode,
        host: Arc<H>,
        credential: Arc<C>,
        base: Option<&str>,
    ) -> Result<Self, InitError> {
        if config.model.as_ref().is_none_or(|model| model.trim().is_empty()) {
            return Err(InitError::Model);
        }
        Ok(Self {
            transport: transport::Transport::new(mode, base)?,
            budget: Arc::new(budget::Budget::new(config.daily_request_cap)),
            config,
            host,
            credential,
            tasks: Mutex::new(HashMap::new()),
            sequence: AtomicU64::new(0),
        })
    }
}

impl<H: ToolHost + Send + Sync + 'static, C: ModelCredential + Send + Sync + 'static> AgentBackend
    for ResponsesBackend<H, C>
{
    fn submit(&self, task: Task) -> impl Future<Output = Result<TaskId, AgentError>> + Send {
        let deadline = tokio::time::Instant::now().checked_add(std::time::Duration::from_secs(
            task.deadline_seconds.min(self.config.task_deadline_seconds),
        ));
        let cap = task.max_tool_calls.min(self.config.max_tool_calls);
        std::future::ready(
            deadline
                .ok_or(AgentError::UnscriptedTask)
                .and_then(|deadline| self.start(task, Some((deadline, cap)))),
        )
    }

    async fn next_event(&self, id: &TaskId) -> Option<TaskEvent> {
        let events = lock(&self.tasks).get(id).cloned().flatten()?;
        let event = events.next().await;
        if !matches!(event, Some(TaskEvent::Progress { .. })) {
            lock(&self.tasks).insert(id.clone(), None);
        }
        event
    }

    async fn cancel(&self, id: &TaskId) -> Result<(), AgentError> {
        let events = lock(&self.tasks).get(id).cloned().ok_or(AgentError::UnknownTask)?;
        if let Some(events) = events {
            events.cancel(id).await;
        }
        Ok(())
    }
}

impl<H: ToolHost + Send + Sync + 'static, C: ModelCredential + Send + Sync + 'static> ResponsesBackend<H, C> {
    /// Submit work whose lifetime and cancellation belong to the bridge's run.
    /// # Errors
    /// Returns an error if the backend cannot allocate a task identifier.
    pub fn submit_for_run(&self, task: Task) -> Result<TaskId, AgentError> {
        self.start(task, None)
    }

    fn start(&self, task: Task, limits: Option<(tokio::time::Instant, u32)>) -> Result<TaskId, AgentError> {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let id = TaskId::new(format!("task-{sequence}")).map_err(|_| AgentError::UnscriptedTask)?;
        let events = Arc::new(events::Events::new(&id));
        let worker_events = events.clone();
        let host = self.host.clone();
        let credential = self.credential.clone();
        let transport = self.transport.clone();
        let config = self.config.clone();
        let budget = self.budget.clone();
        let worker_id = id.clone();
        let worker = tokio::spawn(async move {
            let run = conversation::run(
                &worker_id,
                task,
                (&config, limits.map(|(_, cap)| cap)),
                &transport,
                &*host,
                &*credential,
                &budget,
            );
            let result = worker_events.drive(run, limits.map(|(deadline, _)| deadline)).await;
            worker_events.finish(&worker_id, result);
        });
        events.set_worker(worker);
        lock(&self.tasks).insert(id.clone(), Some(events));
        Ok(id)
    }
}

impl<H, C> Drop for ResponsesBackend<H, C> {
    fn drop(&mut self) {
        for events in lock(&self.tasks).values().flatten() {
            events.stop();
        }
    }
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_blank_model_fails_without_a_credential_adapter_or_connection() {
        for model in [None, Some(String::new()), Some("  \n".into())] {
            let config = AgentConfig {
                model,
                max_tool_calls: 8,
                task_deadline_seconds: 60,
                daily_request_cap: 500,
            };
            // Unit types cannot provide credentials; construction must not request any.
            let result = ResponsesBackend::new(config, LiveMode::Chatgpt, Arc::new(()), Arc::new(()));
            assert!(matches!(result, Err(InitError::Model)));
            assert_eq!(
                result.err().unwrap().to_string(),
                "agent.model: explicit nonblank model required"
            );
        }
    }
}

#[cfg(test)]
mod backend_tests;
