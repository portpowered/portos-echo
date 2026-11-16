//! One runtime grant owner; operator processes never open the durable store.
pub mod bridge_clock;
#[cfg(test)]
mod bridge_component;
mod bridge_control;
mod bridge_events;
mod bridge_http;
mod bridge_operations;
pub(crate) mod bridge_presentation;
mod bridge_settings;
mod bridge_token;

use crate::Error;
pub use bridge_control::{Command, Prepared, parse};
use bridge_presentation::Presentation;
pub use bridge_settings::Settings;
pub(crate) use bridge_token::Token;
use echo_mcp_auth::{AuthClient, AuthError};
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::{sync::mpsc, task::JoinHandle};

pub(crate) struct Grant {
    client: AuthClient,
    rejected: AtomicBool,
    presentation: Presentation,
    failed: AtomicBool,
    epoch: u64,
    started: tokio::time::Instant,
    trace: Mutex<crate::bridge_trace::Trace>,
    trace_failed: AtomicBool,
    operation: AtomicU64,
    reloaded: AtomicBool,
}

impl Grant {
    fn now(&self) -> u64 {
        self.epoch.saturating_add(self.started.elapsed().as_secs())
    }
    fn reject(&self, message: &str) {
        self.rejected.store(true, Ordering::Release);
        if self.presentation.rejected(message).is_err() {
            self.failed.store(true, Ordering::Release);
        }
    }

    pub fn blocked(&self) -> bool {
        self.rejected.load(Ordering::Acquire)
    }

    pub fn check(&self) -> Result<(), Error> {
        if self.trace_failed.load(Ordering::Acquire) {
            Err(Error::Trace)
        } else if self.failed.load(Ordering::Acquire) {
            Err(Error::Screen)
        } else {
            Ok(())
        }
    }
}

pub(crate) struct Runtime {
    pub grant: Arc<Grant>,
    pub address: SocketAddr,
    tasks: Vec<JoinHandle<()>>,
}

impl Runtime {
    pub async fn open(
        settings: Settings,
        presentation: Presentation,
        trace: crate::bridge_trace::Trace,
    ) -> Result<Self, Error> {
        let token = bridge_control::pairing(&settings.pairing_file)?;
        let listener = tokio::net::TcpListener::bind(settings.listen)
            .await
            .map_err(|_| Error::Control)?;
        let address = listener.local_addr().map_err(|_| Error::Control)?;
        let grant = Arc::new(Grant {
            client: AuthClient::open(settings.options).await.map_err(Error::Mcp)?,
            rejected: AtomicBool::new(false),
            presentation,
            failed: AtomicBool::new(false),
            epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| Error::Control)?
                .as_secs(),
            started: tokio::time::Instant::now(),
            trace: Mutex::new(trace),
            trace_failed: AtomicBool::new(false),
            operation: AtomicU64::new(0),
            reloaded: AtomicBool::new(true),
        });
        let (send, receive) = mpsc::channel(1);
        let tasks = vec![
            tokio::spawn(bridge_operations::serve(grant.clone(), receive)),
            tokio::spawn(bridge_http::serve(listener, token, send)),
        ];
        Ok(Self { grant, address, tasks })
    }

    pub async fn close(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        for task in &mut self.tasks {
            let _ = task.await;
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

pub(crate) fn category(error: &AuthError) -> &'static str {
    match error {
        AuthError::Discovery => "discovery",
        AuthError::Transport => "transport",
        AuthError::Protocol => "protocol",
        AuthError::AccessDenied => "access_denied",
        AuthError::Expired => "expired",
        AuthError::RelinkRequired => "relink_required",
        AuthError::Storage => "storage",
    }
}
