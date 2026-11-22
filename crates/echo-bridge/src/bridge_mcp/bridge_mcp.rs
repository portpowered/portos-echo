//! One runtime grant owner; operator processes never open the durable store.
pub mod bridge_clock;
#[cfg(test)]
pub(crate) mod bridge_component;
mod bridge_control;
mod bridge_events;
mod bridge_http;
pub(crate) mod bridge_operations;
pub(crate) mod bridge_presentation;
mod bridge_settings;
mod bridge_token;

use crate::Error;
pub use bridge_control::{Command, Prepared, parse};
use bridge_presentation::Presentation;
pub use bridge_settings::Settings;
pub(crate) use bridge_token::Token;
use echo_mcp_auth::{AuthClient, AuthError, GrantStatus};
use std::{
    collections::HashSet,
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
    scopes: Mutex<ScopeStatus>,
    operator: bridge_operations::Operator,
    screen_operation: Mutex<bridge_operations::ScreenOperation>,
}

#[derive(Default)]
struct ScopeStatus {
    generation: u64,
    denials: HashSet<(String, &'static str)>,
}

impl Grant {
    pub(crate) fn endpoint_grant(&self, api_base: &str) -> Result<echo_mcp_auth::EndpointGrant<'_>, AuthError> {
        self.client.endpoint_grant(api_base)
    }

    /// Why this grant's sign-in does not cover the endpoint audience and
    /// scopes, as a fixed reason; `None` when it does.
    pub(crate) async fn endpoint_unready(&self, api_base: &str) -> Option<&'static str> {
        self.client.endpoint_unready(api_base).await
    }

    fn now(&self) -> u64 {
        self.epoch.saturating_add(self.started.elapsed().as_secs())
    }
    fn reject(&self, message: &str) {
        self.rejected.store(true, Ordering::Release);
        if self.presentation.rejected(message).is_err() {
            self.failed.store(true, Ordering::Release);
        }
    }

    pub fn link_status(&self) -> echo_screen_contract::settings_v1::LinkStatusV1 {
        use echo_screen_contract::settings_v1::LinkStatusV1;
        if !self.blocked() && self.client.has_grant() {
            LinkStatusV1::Linked
        } else {
            LinkStatusV1::LinkRequired
        }
    }

    /// Project only the usable grant scopes from the existing owner.
    pub(crate) async fn granted_scopes(&self) -> Option<Vec<String>> {
        if self.blocked() {
            return None;
        }
        self.grant_status().await.scopes
    }

    /// Read the usable committed grant for Settings without exposing its identity.
    pub(crate) async fn grant_status(&self) -> GrantStatus {
        let mut status = self.client.grant_status().await;
        // Linking blocks access while the previous grant is still committed.
        if self.blocked() {
            status.scopes = None;
        }
        status
    }

    pub fn blocked(&self) -> bool {
        self.rejected.load(Ordering::Acquire)
    }

    pub fn scope_generation(&self) -> Result<u64, Error> {
        self.scopes
            .lock()
            .map(|state| state.generation)
            .map_err(|_| Error::Control)
    }

    pub fn scope_denied(&self, generation: u64, tool: &str, scope: &'static str) -> Result<(), Error> {
        let mut state = self.scopes.lock().map_err(|_| Error::Control)?;
        if state.generation == generation && !self.blocked() && state.denials.insert((tool.into(), scope)) {
            self.event(
                "mcp.scope_insufficient",
                serde_json::json!({"tool":tool,"missing_scope":scope}),
            )
            .map_err(Error::Mcp)?;
        }
        Ok(())
    }

    pub fn health_status(&self) -> &'static str {
        if self.blocked() || !self.client.has_grant() {
            "unlinked"
        } else if self.scopes.lock().is_ok_and(|state| !state.denials.is_empty()) {
            "relink_for_scope"
        } else {
            "linked"
        }
    }

    fn reset_scope_status(&self) -> Result<(), AuthError> {
        let mut state = self.scopes.lock().map_err(|_| AuthError::Storage)?;
        state.generation = state.generation.checked_add(1).ok_or(AuthError::Storage)?;
        state.denials.clear();
        Ok(())
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
    pub operator: bridge_operations::Operator,
    tasks: Vec<JoinHandle<()>>,
}

impl Runtime {
    pub async fn open(
        settings: Settings,
        presentation: Presentation,
        trace: crate::bridge_trace::Trace,
        epoch: Option<u64>,
    ) -> Result<Self, Error> {
        let token = bridge_control::pairing(&settings.pairing_file)?;
        let listener = tokio::net::TcpListener::bind(settings.listen)
            .await
            .map_err(|_| Error::Control)?;
        let address = listener.local_addr().map_err(|_| Error::Control)?;
        let (send, receive) = mpsc::channel(1);
        let operator = bridge_operations::Operator::new(send);
        let grant = Arc::new(Grant {
            client: AuthClient::open_configured(settings.options, epoch, settings.ca_pem.as_deref())
                .await
                .map_err(Error::Mcp)?,
            rejected: AtomicBool::new(false),
            presentation,
            failed: AtomicBool::new(false),
            epoch: match epoch {
                Some(epoch) => epoch,
                None => std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| Error::Control)?
                    .as_secs(),
            },
            started: tokio::time::Instant::now(),
            trace: Mutex::new(trace),
            trace_failed: AtomicBool::new(false),
            operation: AtomicU64::new(0),
            reloaded: AtomicBool::new(true),
            scopes: Mutex::default(),
            operator: operator.clone(),
            screen_operation: Mutex::default(),
        });
        let mut runtime = Self {
            grant,
            address,
            operator,
            tasks: Vec::new(),
        };
        runtime.tasks = vec![
            tokio::spawn(bridge_operations::serve(runtime.grant.clone(), receive)),
            tokio::spawn(bridge_http::serve(listener, token, runtime.operator.clone())),
        ];
        Ok(runtime)
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
        AuthError::RegistrationUnavailable => "registration_unavailable",
        AuthError::RegistrationRefused => "registration_refused",
        AuthError::Transport => "transport",
        AuthError::Protocol => "protocol",
        AuthError::AccessDenied => "access_denied",
        AuthError::Expired => "expired",
        AuthError::RelinkRequired => "relink_required",
        AuthError::Storage => "storage",
    }
}
