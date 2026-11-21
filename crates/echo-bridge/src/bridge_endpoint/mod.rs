//! Endpoint ownership is scoped to the configured Settings generation.
mod commands;
pub(crate) use commands::refusal;
pub(crate) mod remote_start;
mod snapshot;
#[path = "speaker/control.rs"]
mod speaker;
#[cfg(test)]
mod tests;
mod transport;
pub(crate) mod voice;

use crate::bridge_mcp::Grant;
use echo_config::{BridgeConfig, McpAuth};
use echo_mcp_auth::{AuthError, EndpointExchange, EndpointRegistration, EndpointTokenSource, ExchangeError};
use std::{
    future::{Future, pending},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::{sync::watch, time::sleep};
use transport::{REQUEST_TIMEOUT, Transport};

/// Route read-back retries: Port OS discovers the paired route asynchronously.
const DISCOVERY_DELAYS: [u64; 6] = [0, 5, 30, 120, 600, 1800];

pub(crate) const UNAVAILABLE: &str = "Port OS endpoint connection is unavailable.";

#[derive(Clone)]
enum Status {
    Off,
    Connecting,
    Connected,
    Unavailable,
    Relink,
    ClearFailed,
}

#[derive(Clone, PartialEq, Eq)]
struct Desired {
    name: Option<String>,
    unlink: u64,
}

pub(crate) struct Control {
    desired: watch::Sender<Desired>,
    status: watch::Receiver<Status>,
}

impl Control {
    pub(crate) fn accept(&self, config: &echo_config::PortosEndpoint) {
        let name = config.enabled.then(|| config.device_name.clone());
        self.desired.send_if_modified(|desired| {
            if desired.name == name {
                return false;
            }
            desired.name = name;
            true
        });
    }

    pub(crate) fn unlink(&self) {
        self.desired.send_modify(|desired| {
            desired.name = None;
            desired.unlink = desired.unlink.wrapping_add(1);
        });
    }

    pub(crate) fn present(&self, snapshot: &mut echo_screen_contract::settings_v2::SettingsSnapshotV2) {
        snapshot.warnings.retain(|warning| warning != UNAVAILABLE);
        let status = self.status.borrow();
        let warning = match &*status {
            Status::Off => return,
            Status::Connecting => "Connecting this Echo to Port OS.",
            Status::Connected => "This Echo is connected to Port OS.",
            Status::Unavailable => UNAVAILABLE,
            Status::Relink => "Link this Echo with its own Port OS client to connect the endpoint.",
            Status::ClearFailed => "Port OS endpoint data could not be cleared.",
        };
        snapshot.warnings.push(warning.into());
    }
}

pub(crate) struct Runtime {
    home: PathBuf,
    api_base: String,
    exchange: EndpointExchange,
    transport: Transport,
    desired: watch::Receiver<Desired>,
    status: watch::Sender<Status>,
}

impl Runtime {
    pub(crate) fn configured(config: &BridgeConfig) -> Option<(Self, Control)> {
        let api = endpoint_api(config)?;
        let gateway = config.portos_endpoint.gateway_for(&api)?;
        let exchange = EndpointExchange::open(&api, REQUEST_TIMEOUT).ok()?;
        let transport = Transport::new(&gateway, &api, false)?;
        let home = config.mcp.state_dir.clone()?;
        Some(Self::new(home, api, exchange, transport, &config.portos_endpoint))
    }

    fn new(
        home: PathBuf,
        api_base: String,
        exchange: EndpointExchange,
        transport: Transport,
        initial: &echo_config::PortosEndpoint,
    ) -> (Self, Control) {
        let (desired_send, desired) = watch::channel(Desired { name: None, unlink: 0 });
        let (status, status_view) = watch::channel(if initial.enabled {
            Status::Connecting
        } else {
            Status::Off
        });
        let control = Control {
            desired: desired_send,
            status: status_view,
        };
        control.accept(initial);
        (
            Self {
                home,
                api_base,
                exchange,
                transport,
                desired,
                status,
            },
            control,
        )
    }

    pub(crate) async fn scope<T>(
        mut self,
        work: impl Future<Output = T>,
        current_grant: impl Fn() -> Option<Arc<Grant>>,
        trace: Option<crate::bridge_trace::Trace>,
    ) -> T {
        self.transport.trace = trace;
        voice::CURRENT
            .scope(voice::Voice::default(), self.supervise(work, current_grant))
            .await
    }

    async fn supervise<T>(self, work: impl Future<Output = T>, current_grant: impl Fn() -> Option<Arc<Grant>>) -> T {
        let mut unlinked = 0;
        let mut store = None;
        let mut changes = self.desired.clone();
        tokio::pin!(work);
        loop {
            let desired = changes.borrow_and_update().clone();
            if desired.unlink != unlinked {
                self.clear(&mut store).await;
                unlinked = desired.unlink;
            }
            let active = self.activate(desired.name.as_deref(), &mut store, &current_grant);
            let result = tokio::select! {
                biased;
                result = &mut work => Some(result),
                _ = changes.changed() => None,
                () = active => None,
            };
            if let Some(result) = result {
                let unlink = changes.borrow().unlink;
                if unlink != unlinked {
                    self.clear(&mut store).await;
                }
                return result;
            }
            if changes.has_changed().is_err() {
                return work.await;
            }
        }
    }

    async fn clear(&self, store: &mut Option<Arc<EndpointRegistration>>) {
        let status = match self.store(store) {
            Ok(store) if store.unlink().await.is_ok() => Status::Off,
            _ => Status::ClearFailed,
        };
        self.status.send_replace(status);
    }

    fn store(&self, slot: &mut Option<Arc<EndpointRegistration>>) -> Result<Arc<EndpointRegistration>, AuthError> {
        if let Some(store) = slot {
            return Ok(store.clone());
        }
        let store = Arc::new(EndpointRegistration::open(&self.home)?);
        *slot = Some(store.clone());
        Ok(store)
    }

    async fn activate(
        &self,
        name: Option<&str>,
        store: &mut Option<Arc<EndpointRegistration>>,
        current_grant: &impl Fn() -> Option<Arc<Grant>>,
    ) {
        if let Some(name) = name {
            self.status.send_replace(Status::Connecting);
            let status = match self.store(store) {
                Ok(store) => self
                    .pair(name, &store, current_grant)
                    .await
                    .unwrap_or_else(|error| auth_status(&error)),
                Err(error) => auth_status(&error),
            };
            self.status.send_replace(status);
        } else {
            self.status.send_if_modified(|status| {
                if matches!(status, Status::Off | Status::ClearFailed) {
                    return false;
                }
                *status = Status::Off;
                true
            });
        }
        pending::<()>().await;
    }

    async fn pair(
        &self,
        name: &str,
        store: &Arc<EndpointRegistration>,
        current_grant: &impl Fn() -> Option<Arc<Grant>>,
    ) -> Result<Status, AuthError> {
        let owner = linked_owner(current_grant, &self.api_base, &self.status).await;
        let tokens = owner.endpoint_grant(&self.api_base)?;
        // A pending request may refer to the previous name. An accepted rename
        // follows that checkpoint rather than altering the old key's body.
        while let Some(exchange) = store.exchange(name).await? {
            let result = self.exchange.submit(&tokens, exchange.key(), &exchange.request()).await;
            match result {
                Ok(result) => store.complete_exchange(exchange, result.auth_link_id).await?,
                Err(ExchangeError::Transport) => sleep(Duration::from_secs(1)).await,
                Err(ExchangeError::Grant) if owner.health_status() == "unlinked" => return Ok(Status::Relink),
                Err(_) => return Ok(Status::Unavailable),
            }
        }
        let install = store.installation().await.ok_or(AuthError::Storage)?;
        // Only a refresh after pairing mints tokens with the auth-link claim.
        tokens.refresh_installation().await?;
        let device_id = store.device_id().await;
        let serve = self
            .transport
            .serve(&install.auth_link_id, &device_id, &tokens, &self.status);
        tokio::pin!(serve);
        if install.endpoint_id.is_none() {
            tokio::select! {
                status = &mut serve => return Ok(status),
                () = self.discover(store, &install.auth_link_id, &device_id, &tokens) => {}
            }
        }
        Ok(serve.await)
    }

    /// Read the paired route back for display and logs. Commands and reports
    /// address the `deviceId`, so the bridge serves while discovery lags.
    async fn discover(
        &self,
        store: &EndpointRegistration,
        auth_link_id: &str,
        device_id: &str,
        tokens: &dyn EndpointTokenSource,
    ) {
        for delay in DISCOVERY_DELAYS {
            sleep(Duration::from_secs(delay)).await;
            match self.exchange.endpoint_id(tokens, device_id).await {
                Ok(Some(endpoint_id)) => {
                    let stored = store.remember_endpoint(auth_link_id, endpoint_id).await.is_ok();
                    self.discovered(if stored { "found" } else { "unsaved" });
                    return;
                }
                Ok(None) | Err(ExchangeError::Transport) => {}
                Err(_) => break,
            }
        }
        self.discovered("unknown");
    }

    fn discovered(&self, result: &'static str) {
        if let Some(trace) = &self.transport.trace {
            let _ = trace
                .clone()
                .record("endpoint.route", None, serde_json::json!({ "result": result }));
        }
    }
}

fn endpoint_api(config: &BridgeConfig) -> Option<String> {
    if let Some(api) = &config.portos_endpoint.api_base {
        return Some(api.clone());
    }
    // This is the same first server that supplies the configured Port OS grant.
    let server = config
        .mcp
        .server
        .first()
        .filter(|server| server.auth == McpAuth::PortosDeviceCode)?;
    let resource = reqwest::Url::parse(&server.url).ok()?;
    (resource.scheme() == "https").then(|| resource.origin().ascii_serialization())
}

/// Wait for a linked grant whose sign-in covers the endpoint: both audiences
/// and the endpoint scopes. An older grant shows Relink until it is relinked.
async fn linked_owner(
    current_grant: &impl Fn() -> Option<Arc<Grant>>,
    api_base: &str,
    status: &watch::Sender<Status>,
) -> Arc<Grant> {
    loop {
        if let Some(owner) = current_grant()
            && owner.link_status() == echo_screen_contract::settings_v1::LinkStatusV1::Linked
            && owner.endpoint_ready(api_base).await
        {
            status.send_replace(Status::Connecting);
            return owner;
        }
        status.send_if_modified(|value| {
            let changed = !matches!(value, Status::Relink);
            *value = Status::Relink;
            changed
        });
        sleep(Duration::from_millis(250)).await;
    }
}

fn auth_status(error: &AuthError) -> Status {
    match error {
        AuthError::RelinkRequired => Status::Relink,
        _ => Status::Unavailable,
    }
}
