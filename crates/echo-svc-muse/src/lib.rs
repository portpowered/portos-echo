//! The direct Muse gadget connection and its local service boundary.
#![cfg(target_os = "linux")]

mod api;
mod chat;
mod clock;
mod control;
mod link;
mod noise;
mod pairing;
mod tasks;
mod trust;
mod wire;

#[cfg(test)]
mod tests;

use std::{future::Future, path::Path, sync::Arc};

use echo_agent_api::Task;
use echo_contracts::{ids::TaskId, task::TaskEvent};
use tokio::{
    sync::{Mutex, mpsc, oneshot, watch},
    task::JoinHandle,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceState {
    Disconnected,
    Connecting,
    Registered,
    PairingRemoved,
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ServiceError {
    #[error("Muse pairing is unavailable")]
    PairingUnavailable,
    #[error("Muse pairing cannot be used safely")]
    UnsafePairing,
    #[error("Muse trust store cannot be used safely")]
    TrustUnavailable,
    #[error("Muse service is not ready")]
    NotReady,
    #[error("Muse peer is unavailable")]
    PeerUnavailable,
    #[error("unknown Muse task")]
    UnknownTask,
    #[error("invalid Muse task deadline")]
    InvalidDeadline,
    #[error("Muse private storage failed")]
    Storage,
}

pub trait MuseService {
    fn state(&self) -> ServiceState;
    fn submit(&self, task: Task) -> impl Future<Output = Result<TaskId, ServiceError>> + Send + '_;
    fn next_event<'a>(
        &'a self,
        id: &'a TaskId,
    ) -> impl Future<Output = Result<Option<TaskEvent>, ServiceError>> + Send + 'a;
    fn cancel<'a>(&'a self, id: &'a TaskId) -> impl Future<Output = Result<(), ServiceError>> + Send + 'a;
    fn stop(&self) -> impl Future<Output = Result<(), ServiceError>> + Send + '_;
}

/// Owns a single pairing and its registered link until stopped or dropped.
pub struct MuseLink {
    state: watch::Receiver<(ServiceState, Option<ServiceError>)>,
    endpoint: watch::Receiver<Option<mpsc::Sender<chat::Command>>>,
    stop: watch::Sender<bool>,
    worker: Mutex<Option<JoinHandle<()>>>,
    tasks: Arc<tasks::Tasks>,
}

impl MuseLink {
    /// Admit the private pairing, task snapshot and caller-supplied PEM roots, then begin registration.
    ///
    /// # Errors
    /// Returns an admission error before starting any network work if a private file or the roots are unsafe.
    /// Returns `NotReady` when called outside a Tokio runtime.
    pub fn start(pairing_path: &Path, task_path: &Path, ca_pem: &[u8]) -> Result<Self, ServiceError> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| ServiceError::NotReady)?;
        let pairing = pairing::Pairing::open(pairing_path)?;
        let connector = api::Network::new(trust::client_config(ca_pem)?)?;
        let tasks = tasks::Tasks::open(task_path, clock::EpochClock, link::OsRandom::new())?;
        Ok(Self::spawn(&runtime, pairing, connector, tasks))
    }

    fn spawn(
        runtime: &tokio::runtime::Handle,
        mut pairing: pairing::Pairing,
        connector: impl api::Connector,
        tasks: tasks::Tasks,
    ) -> Self {
        let (state_sink, state) = watch::channel((ServiceState::Connecting, None));
        let (endpoint_sink, endpoint) = watch::channel(None);
        let (stop, stop_requested) = watch::channel(false);
        let tasks = Arc::new(tasks);
        let worker_tasks = Arc::clone(&tasks);
        let worker = runtime.spawn(async move {
            let result = link::run(
                &mut pairing,
                &connector,
                &worker_tasks,
                &state_sink,
                &endpoint_sink,
                stop_requested,
            )
            .await;
            drop(pairing);
            let status = match result {
                Ok(status) => (status, None),
                Err(error) => (ServiceState::Disconnected, Some(error)),
            };
            state_sink.send_replace(status);
        });
        Self {
            state,
            endpoint,
            stop,
            worker: Mutex::new(Some(worker)),
            tasks,
        }
    }

    fn connected(&self) -> Option<mpsc::Sender<chat::Command>> {
        if *self.stop.borrow() || self.state() != ServiceState::Registered {
            return None;
        }
        self.endpoint.borrow().clone()
    }

    /// Wait for the VM to acknowledge this instance's registration attempt.
    ///
    /// # Errors
    /// Returns the redacted link failure or `NotReady` if the service was stopped.
    pub async fn wait_registered(&self) -> Result<(), ServiceError> {
        let mut state = self.state.clone();
        let (current, error) = *state
            .wait_for(|(state, _)| *state != ServiceState::Connecting)
            .await
            .map_err(|_| ServiceError::NotReady)?;
        if current == ServiceState::Registered {
            Ok(())
        } else {
            Err(error.unwrap_or(ServiceError::NotReady))
        }
    }
}

impl MuseService for MuseLink {
    fn state(&self) -> ServiceState {
        self.state.borrow().0
    }

    async fn submit(&self, task: Task) -> Result<TaskId, ServiceError> {
        let endpoint = self.connected().ok_or(ServiceError::NotReady)?;
        let submission = self.tasks.prepare(&task)?;
        let permit = endpoint.reserve().await.map_err(|_| ServiceError::NotReady)?;
        if *self.stop.borrow() {
            return Err(ServiceError::NotReady);
        }
        self.tasks.admit(&submission)?;
        let id = submission.id.clone();
        let (receipt, outcome) = oneshot::channel();
        permit.send(chat::Command::Submit(submission, receipt));
        // If the link disappears before acknowledging the send, the committed
        // task can still receive a report and must not be sent again.
        if let Ok(result) = outcome.await {
            result?;
        }
        Ok(id)
    }

    async fn next_event(&self, id: &TaskId) -> Result<Option<TaskEvent>, ServiceError> {
        self.tasks.next_event(id).await
    }

    async fn cancel(&self, id: &TaskId) -> Result<(), ServiceError> {
        if let Some(session) = self.tasks.cancel(id)?
            && let Some(endpoint) = self.connected()
        {
            let _ = endpoint
                .send(chat::Command::Cancel {
                    id: id.clone(),
                    session,
                })
                .await;
        }
        Ok(())
    }

    async fn stop(&self) -> Result<(), ServiceError> {
        self.stop.send_replace(true);
        let mut worker = self.worker.lock().await;
        if let Some(join) = worker.as_mut() {
            let result = join.await;
            *worker = None;
            result.map_err(|_| ServiceError::PeerUnavailable)?;
        }
        Ok(())
    }
}
