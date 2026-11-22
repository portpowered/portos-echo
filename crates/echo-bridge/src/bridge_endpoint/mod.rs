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

#[derive(Clone, Debug, PartialEq, Eq)]
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
    #[cfg(target_os = "android")]
    pub(crate) fn bind_camera(&mut self, exchange: echo_screen_server::Camera, id: String) {
        self.transport.camera.source = Some(Arc::new(snapshot::NativeCamera::new(exchange, id)));
    }

    /// The endpoint runtime for this configuration, or the fixed reason it has none.
    /// Its REST client and gateway trust the bundle the Port OS grant trusts.
    pub(crate) fn configured(config: &BridgeConfig) -> Result<(Self, Control), &'static str> {
        let api = endpoint_api(config).ok_or("no_endpoint_api")?;
        let gateway = config.portos_endpoint.gateway_for(&api).ok_or("no_gateway")?;
        let ca_pem = crate::bridge_mcp::Settings::ca_pem(config).map_err(|_| "ca_unavailable")?;
        let exchange =
            EndpointExchange::open(&api, REQUEST_TIMEOUT, ca_pem.as_deref()).map_err(|_| "endpoint_api_refused")?;
        let transport = Transport::new(&gateway, &api, false, ca_pem.as_deref()).ok_or("gateway_refused")?;
        let home = config.mcp.state_dir.clone().ok_or("no_state_dir")?;
        Ok(Self::new(home, api, exchange, transport, &config.portos_endpoint))
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
        let observed = observe(self.status.subscribe(), trace.clone());
        self.transport.trace = trace;
        let supervised = voice::CURRENT.scope(voice::Voice::default(), self.supervise(work, current_grant));
        tokio::pin!(supervised);
        tokio::select! {
            biased;
            result = &mut supervised => result,
            () = observed => supervised.await,
        }
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
            let paired = match self.store(store) {
                Ok(store) => self.pair(name, &store, current_grant).await,
                Err(error) => Err(error),
            };
            let status = paired.unwrap_or_else(|error| {
                let failed = serde_json::json!({ "reason": auth_reason(&error) });
                note(self.transport.trace.as_ref(), "endpoint.pair_failed", failed);
                auth_status(&error)
            });
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
        let trace = self.transport.trace.as_ref();
        let owner = linked_owner(current_grant, &self.api_base, &self.status, trace).await;
        let tokens = owner.endpoint_grant(&self.api_base)?;
        if let Some(status) = self.exchange_pending(name, store, &owner, &tokens).await? {
            return Ok(status);
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

    /// Send the committed exchange until Port OS answers it. An unanswered
    /// request or a server error is sent again unchanged, with backoff, while
    /// Settings shows the endpoint unavailable. `Some` ends pairing with that status.
    async fn exchange_pending(
        &self,
        name: &str,
        store: &EndpointRegistration,
        owner: &Grant,
        tokens: &dyn EndpointTokenSource,
    ) -> Result<Option<Status>, AuthError> {
        let trace = self.transport.trace.as_ref();
        let mut failures = Attempts::default();
        // A pending request may refer to the previous name. An accepted rename
        // follows that checkpoint rather than altering the old key's body.
        while let Some(exchange) = store.exchange(name).await? {
            if failures.count == 0 {
                let rename = exchange.request().auth_link_id().is_some();
                note(trace, "endpoint.exchanging", serde_json::json!({ "rename": rename }));
            }
            let error = match self.exchange.submit(tokens, exchange.key(), &exchange.request()).await {
                Ok(result) => {
                    store.complete_exchange(exchange, result.auth_link_id).await?;
                    let attempts = failures.count.saturating_add(1);
                    note(trace, "endpoint.exchanged", serde_json::json!({ "attempts": attempts }));
                    continue;
                }
                Err(error) => error,
            };
            let retry = error.retryable();
            if let Some(attempt) = failures.failed(error.reason()) {
                let failed = serde_json::json!({
                    "reason": error.reason(), "http_class": error.http_status().map(http_class),
                    "http_status": error.http_status(), "attempt": attempt, "retry": retry,
                });
                note(trace, "endpoint.exchange_failed", failed);
            }
            if !retry {
                let relink = error == ExchangeError::Grant && owner.health_status() == "unlinked";
                return Ok(Some(if relink { Status::Relink } else { Status::Unavailable }));
            }
            self.status.send_replace(Status::Unavailable);
            sleep(failures.backoff()).await;
        }
        self.status.send_replace(Status::Connecting);
        Ok(None)
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
                Ok(None) => {}
                Err(error) if error.retryable() => {}
                Err(_) => break,
            }
        }
        self.discovered("unknown");
    }

    fn discovered(&self, result: &'static str) {
        note(
            self.transport.trace.as_ref(),
            "endpoint.route",
            serde_json::json!({ "result": result }),
        );
    }
}

/// Record one endpoint lifecycle event. Fields carry fixed reasons, counts and
/// HTTP statuses only: never ids, names, tokens or URLs.
fn note(trace: Option<&crate::bridge_trace::Trace>, event: &'static str, fields: serde_json::Value) {
    if let Some(trace) = trace {
        let _ = trace.clone().record(event, None, fields);
    }
}

/// Trace every change of the status Settings presents, from its first change.
async fn observe(mut status: watch::Receiver<Status>, trace: Option<crate::bridge_trace::Trace>) {
    let mut last = status.borrow_and_update().clone();
    while status.changed().await.is_ok() {
        let now = status.borrow_and_update().clone();
        if now != last {
            note(
                trace.as_ref(),
                "endpoint.status",
                serde_json::json!({ "status": now.name() }),
            );
            last = now;
        }
    }
    pending::<()>().await;
}

impl Status {
    fn name(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Unavailable => "unavailable",
            Self::Relink => "relink",
            Self::ClearFailed => "clear_failed",
        }
    }
}

/// Consecutive failures of one retried step. A failure is recorded the first
/// time, when its reason changes, and on every power-of-two repeat.
#[derive(Default)]
struct Attempts {
    count: u32,
    reason: Option<&'static str>,
}

impl Attempts {
    /// Count one failure; `Some(attempt)` when it should be recorded.
    fn failed(&mut self, reason: &'static str) -> Option<u32> {
        self.count = self.count.saturating_add(1);
        let changed = self.reason.replace(reason) != Some(reason);
        (changed || self.count.is_power_of_two()).then_some(self.count)
    }

    /// 1 s doubling to 60 s.
    fn backoff(&self) -> Duration {
        let exponent = self.count.saturating_sub(1).min(6);
        Duration::from_secs(1_u64 << exponent).min(Duration::from_secs(60))
    }
}

fn http_class(status: u16) -> String {
    format!("{}xx", status / 100)
}

/// Wait for a linked grant whose sign-in covers the endpoint: both audiences
/// and the endpoint scopes. An older grant shows Relink until it is relinked,
/// and the trace names why once per change of reason.
async fn linked_owner(
    current_grant: &impl Fn() -> Option<Arc<Grant>>,
    api_base: &str,
    status: &watch::Sender<Status>,
    trace: Option<&crate::bridge_trace::Trace>,
) -> Arc<Grant> {
    let mut waiting = None;
    loop {
        let reason = match current_grant() {
            None => "no_grant",
            Some(owner) if owner.link_status() != echo_screen_contract::settings_v1::LinkStatusV1::Linked => "unlinked",
            Some(owner) => match owner.endpoint_unready(api_base).await {
                None => {
                    status.send_replace(Status::Connecting);
                    note(
                        trace,
                        "endpoint.linked",
                        serde_json::json!({ "waited": waiting.is_some() }),
                    );
                    return owner;
                }
                Some(reason) => reason,
            },
        };
        if waiting.replace(reason) != Some(reason) {
            note(
                trace,
                "endpoint.relink_waiting",
                serde_json::json!({ "reason": reason }),
            );
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

/// A fixed reason for a grant or storage failure, for traces.
fn auth_reason(error: &AuthError) -> &'static str {
    match error {
        AuthError::RelinkRequired => "relink_required",
        AuthError::Transport => "token_transport",
        AuthError::Protocol => "token_protocol",
        AuthError::Storage => "storage",
        AuthError::Discovery => "discovery",
        AuthError::RegistrationUnavailable | AuthError::RegistrationRefused => "registration",
        AuthError::AccessDenied | AuthError::Expired => "link",
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
