//! Deterministic MCP 2025-03-26 counterpart, for T3 loopback tests only.
//!
//! `Spoof::start` binds only 127.0.0.1 (port 0 selects an isolated port).
//! Use `snapshot` and `wait_for_calls` for assertions and arrival barriers, then
//! `shutdown` to cancel outstanding waits and join the owned listener.
//! Records are bounded, retained until shutdown, and never evicted.
//! Defaults: 128 arrivals, 16 in-flight handlers, 64 KiB request bodies.
//! Configuration allows at most 4096 arrivals and 64 handlers; all requests have
//! a 60-second resource deadline (503). Unsigned wire waits are not narrowed.
mod spoof_http;
mod spoof_record;
mod spoof_wire;
use echo_contracts::secret::Secret;
pub use spoof_record::{CallRecord, RequestRecord, Snapshot};
pub use spoof_wire::catalog;
use std::sync::{Arc, Mutex};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    None,
    Http500,
    Slow,
    MalformedJsonRpc,
}

impl Fault {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "http_500" => Some(Self::Http500),
            "slow" => Some(Self::Slow),
            "malformed_json_rpc" => Some(Self::MalformedJsonRpc),
            _ => None,
        }
    }
}

pub struct Options {
    pub port: u16,
    pub fault: Fault,
    pub slow_ms: u64,
    pub expected_bearer: Option<Secret<String>>,
    pub max_records: usize,
    pub max_in_flight: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            port: 0,
            fault: Fault::None,
            slow_ms: 1000,
            expected_bearer: None,
            max_records: 128,
            max_in_flight: 16,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SpoofError {
    #[error("invalid spoof configuration")]
    Configuration,
    #[error("spoof bind failed")]
    Bind,
    #[error("spoof state unavailable")]
    State,
    #[error("spoof task failed")]
    Task,
}

struct State {
    options: Options,
    fault: Mutex<Fault>,
    records: Mutex<Snapshot>,
    arrivals: watch::Sender<usize>,
    stopped: watch::Receiver<bool>,
    slots: Semaphore,
}

pub struct Spoof {
    url: String,
    state: Arc<State>,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<std::io::Result<()>>>,
}

impl Spoof {
    /// # Errors
    /// Invalid limits or bind failure return fixed diagnostics without configuration text.
    pub async fn start(options: Options) -> Result<Self, SpoofError> {
        if options.max_records == 0
            || options.max_records > 4096
            || options.max_in_flight == 0
            || options.max_in_flight > 64
        {
            return Err(SpoofError::Configuration);
        }
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, options.port))
            .await
            .map_err(|_| SpoofError::Bind)?;
        let addr = listener.local_addr().map_err(|_| SpoofError::Bind)?;
        let (stop, stopped) = watch::channel(false);
        let (arrivals, _) = watch::channel(0);
        let state = Arc::new(State {
            fault: Mutex::new(options.fault),
            slots: Semaphore::new(options.max_in_flight),
            options,
            records: Mutex::new(Snapshot::default()),
            arrivals,
            stopped: stopped.clone(),
        });
        let app = spoof_http::router(state.clone());
        let task = tokio::spawn(async move {
            let mut stopped = stopped;
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stopped.wait_for(|value| *value).await;
                })
                .await
        });
        Ok(Self {
            url: format!("http://{addr}/mcp"),
            state,
            stop,
            task: Some(task),
        })
    }

    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// # Errors
    /// A poisoned record lock returns State.
    pub fn snapshot(&self) -> Result<Snapshot, SpoofError> {
        self.state
            .records
            .lock()
            .map(|value| value.clone())
            .map_err(|_| SpoofError::State)
    }

    /// Changes only subsequent tool attempts; never replays an arrival.
    /// # Errors
    /// A poisoned fault lock returns State.
    pub fn set_fault(&self, fault: Fault) -> Result<(), SpoofError> {
        *self.state.fault.lock().map_err(|_| SpoofError::State)? = fault;
        Ok(())
    }

    /// # Errors
    /// A stopped server cannot deliver further arrivals.
    pub async fn wait_for_calls(&self, count: usize) -> Result<(), SpoofError> {
        let mut arrivals = self.state.arrivals.subscribe();
        arrivals
            .wait_for(|value| *value >= count)
            .await
            .map_err(|_| SpoofError::State)?;
        Ok(())
    }

    /// Cancels pending handlers, closes the listener, and joins the server task.
    /// # Errors
    /// Returns Task if serving or joining failed.
    pub async fn shutdown(mut self) -> Result<(), SpoofError> {
        self.stop.send_replace(true);
        if let Some(task) = self.task.take() {
            task.await
                .map_err(|_| SpoofError::Task)?
                .map_err(|_| SpoofError::Task)?;
        }
        Ok(())
    }
}

impl Drop for Spoof {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

#[cfg(test)]
mod spoof_tests;
