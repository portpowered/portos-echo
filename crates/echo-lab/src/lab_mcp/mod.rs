//! Strict section-9 authorization counterpart for isolated T3 M5 sessions.
//! The scenario owns approval and fake UTC; no real principal or service exists.
mod mcp_observations;
mod mcp_proof;
mod mcp_routes;
mod mcp_tokens;
pub use mcp_observations::{LinkCounts, Observations, PortosObservations};

use echo_contracts::secret::Secret;
use echo_spoof_mcp::BearerValidator;
use p256::{ecdsa::SigningKey, elliptic_curve::Generate};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};
use tokio::{sync::watch, task::JoinHandle};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub links: u64,
    pub polls: u64,
    pub refreshes: u64,
    pub replay_rejected: u64,
    pub revokes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Approval {
    #[default]
    Pending,
    Approved,
    Denied,
}

struct State {
    issuer: String,
    resource: String,
    now: u64,
    key: Secret<SigningKey>,
    device_key: Option<Value>,
    scopes: String,
    jtis: BTreeSet<String>,
    approval: Approval,
    device: u64,
    deadline: u64,
    bound: bool,
    generation: u64,
    revoked: bool,
    used_refreshes: BTreeSet<String>,
    counts: Counts,
    #[cfg(test)]
    token_reply: Option<Arc<tokio::sync::Notify>>,
}

type Shared = Arc<Mutex<State>>;

/// One listener and one grant per scenario; dropping cancels owned serving work.
pub struct Fixture {
    base: String,
    shared: Shared,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<std::io::Result<()>>>,
}

fn invalid() -> std::io::Error {
    std::io::Error::other("M5 authorization fixture unavailable")
}

impl Fixture {
    /// # Errors
    /// Returns a fixed local bind category without endpoint or credential data.
    pub async fn bind(epoch: u64) -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| invalid())?;
        let base = format!("http://{}", listener.local_addr().map_err(|_| invalid())?);
        let shared = Arc::new(Mutex::new(State {
            issuer: base.clone(),
            resource: format!("{base}/mcp"),
            now: epoch,
            key: Secret::new(SigningKey::generate()),
            device_key: None,
            scopes: String::new(),
            jtis: BTreeSet::new(),
            approval: Approval::Pending,
            device: 0,
            deadline: 0,
            bound: false,
            generation: 0,
            revoked: true,
            used_refreshes: BTreeSet::new(),
            counts: Counts::default(),
            #[cfg(test)]
            token_reply: None,
        }));
        let app = mcp_routes::router(shared.clone());
        let (stop, mut stopped) = watch::channel(false);
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stopped.wait_for(|value| *value).await;
                })
                .await
        });
        Ok(Self {
            base,
            shared,
            stop,
            task: Some(task),
        })
    }

    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    /// Set the independently bound spoof resource before starting a link.
    /// # Errors
    /// Rejects non-loopback resources or changes after device authorization.
    pub fn set_resource(&self, resource: &str) -> std::io::Result<()> {
        let url = url::Url::parse(resource).map_err(|_| invalid())?;
        if url.scheme() != "http"
            || url.host_str() != Some("127.0.0.1")
            || url.port().is_none_or(|port| port == 0)
            || url.path() != "/mcp"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid());
        }
        let mut state = self.shared.lock().map_err(|_| invalid())?;
        if state.device != 0 {
            return Err(invalid());
        }
        state.resource = resource.into();
        Ok(())
    }

    /// # Errors
    /// A poisoned fixture returns a fixed local error.
    pub fn approve(&self, approval: Approval) -> std::io::Result<()> {
        self.shared.lock().map_err(|_| invalid())?.approval = approval;
        Ok(())
    }

    /// Advance the same UTC epoch used by the bridge simulator clock.
    /// # Errors
    /// Rejects overflow without changing the clock.
    pub fn advance(&self, seconds: u64) -> std::io::Result<()> {
        let mut state = self.shared.lock().map_err(|_| invalid())?;
        state.now = state.now.checked_add(seconds).ok_or_else(invalid)?;
        Ok(())
    }

    /// # Errors
    /// A poisoned fixture returns a fixed local error.
    pub fn counts(&self) -> std::io::Result<Counts> {
        Ok(self.shared.lock().map_err(|_| invalid())?.counts)
    }

    #[must_use]
    pub fn validator(&self) -> Arc<dyn BearerValidator> {
        Arc::new(mcp_tokens::Admission(self.shared.clone()))
    }

    /// Close and join the scenario-owned listener.
    /// # Errors
    /// Reports serving/join failure without private state.
    pub async fn shutdown(mut self) -> std::io::Result<()> {
        self.stop.send_replace(true);
        if let Some(task) = self.task.take() {
            task.await.map_err(|_| invalid())?.map_err(|_| invalid())?;
        }
        Ok(())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

#[cfg(test)]
mod mcp_tests;

#[cfg(test)]
fn approval_task<F>(
    future: F,
    gate: impl std::future::Future<Output = ()> + Send + 'static,
) -> (
    JoinHandle<Result<(), echo_mcp_auth::AuthError>>,
    tokio::sync::oneshot::Receiver<()>,
)
where
    F: std::future::Future<Output = Result<(), echo_mcp_auth::AuthError>> + Send + 'static,
{
    let (ready, registered) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        gate.await;
        tokio::pin!(future);
        let mut ready = Some(ready);
        std::future::poll_fn(|cx| {
            let result = future.as_mut().poll(cx);
            if result.is_pending()
                && let Some(ready) = ready.take()
            {
                let _ = ready.send(());
            }
            result
        })
        .await
    });
    (task, registered)
}
