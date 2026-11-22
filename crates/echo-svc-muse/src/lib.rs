//! The direct Muse gadget connection and its local service boundary.
#![cfg(target_os = "linux")]

mod api;
mod control;
mod link;
mod noise;
mod pairing;
mod trust;
mod wire;

#[cfg(test)]
mod tests;

use std::{future::Future, path::Path};

use echo_agent_api::Task;
use echo_contracts::{ids::TaskId, task::TaskEvent};
use tokio::{
    sync::{Mutex, watch},
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
    stop: watch::Sender<bool>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl MuseLink {
    /// Admit the pairing and caller-supplied PEM roots, then begin registration.
    ///
    /// # Errors
    /// Returns an admission error before starting any network work if the pairing or roots are unsafe.
    /// Returns `NotReady` when called outside a Tokio runtime.
    pub fn start(pairing_path: &Path, ca_pem: &[u8]) -> Result<Self, ServiceError> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| ServiceError::NotReady)?;
        let pairing = pairing::Pairing::open(pairing_path)?;
        let connector = api::Network::new(trust::client_config(ca_pem)?)?;
        Ok(Self::spawn(&runtime, pairing, connector, link::OsRandom::new()))
    }

    fn spawn(
        runtime: &tokio::runtime::Handle,
        pairing: pairing::Pairing,
        connector: impl api::Connector,
        random: impl link::Random,
    ) -> Self {
        let (state_sink, state) = watch::channel((ServiceState::Connecting, None));
        let (stop, mut stop_requested) = watch::channel(false);
        let worker = runtime.spawn(async move {
            // The connection future only borrows the pairing. Release the lease before publishing exit.
            let result = tokio::select! {
                biased;
                _ = stop_requested.changed() => Ok(()),
                result = link::run(&pairing, &connector, &random, &state_sink) => result,
            };
            drop(pairing);
            let status = match result {
                Ok(()) => (ServiceState::Stopped, None),
                Err(error) => (ServiceState::Disconnected, Some(error)),
            };
            state_sink.send_replace(status);
        });
        Self {
            state,
            stop,
            worker: Mutex::new(Some(worker)),
        }
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
        if *self.stop.borrow() {
            ServiceState::Stopped
        } else {
            self.state.borrow().0
        }
    }

    async fn submit(&self, _task: Task) -> Result<TaskId, ServiceError> {
        Err(ServiceError::NotReady)
    }

    async fn next_event(&self, _id: &TaskId) -> Result<Option<TaskEvent>, ServiceError> {
        Err(ServiceError::UnknownTask)
    }

    async fn cancel(&self, _id: &TaskId) -> Result<(), ServiceError> {
        Err(ServiceError::UnknownTask)
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
