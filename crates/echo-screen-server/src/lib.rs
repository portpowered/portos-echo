//! Authenticated, bounded loopback screen transport. LAN exposure requires W3 TLS.
mod screen_assets;
mod screen_core;
mod screen_http;
mod screen_listener;
mod screen_url;

pub use screen_url::page_url;

use echo_contracts::secret::Secret;
use echo_screen_contract::{UiAction, UiState};
use screen_core::Core;
use std::{net::SocketAddr, sync::Arc};
use tokio::{
    net::TcpListener,
    sync::{Mutex, mpsc, watch},
    task::JoinHandle,
};

/// Fixed public diagnostics never contain a token, URL or presentation.
#[derive(Debug, thiserror::Error)]
pub enum ScreenServerError {
    #[error("screen bind must be loopback")]
    InvalidBind,
    #[error("screen pairing token must not be empty")]
    InvalidToken,
    #[error("screen page URL is invalid")]
    InvalidPageUrl,
    #[error("screen listener could not bind")]
    Bind,
    #[error("screen task failed")]
    Task,
}

/// Owns listener lifetime; dropping it cancels the listener and all streams.
pub struct ScreenServer {
    pub bound_addr: SocketAddr,
    pub actions: mpsc::Receiver<UiAction>,
    core: Arc<Mutex<Core>>,
    stop: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

impl ScreenServer {
    /// Close active streams and bar future authorization before returning.
    /// Bytes already delivered to the operating system cannot be recalled.
    ///
    /// # Errors
    /// Reserved for task lifecycle failures.
    pub async fn revoke_pairing(&self) -> Result<(), ScreenServerError> {
        self.core.lock().await.revoke();
        Ok(())
    }

    /// Cancel and join owned tasks, closing streams and the action channel.
    ///
    /// # Errors
    /// Returns `Task` when an owned task failed.
    pub async fn shutdown(&mut self) -> Result<(), ScreenServerError> {
        self.stop.send_replace(true);
        for task in self.tasks.drain(..) {
            task.await.map_err(|_| ScreenServerError::Task)?;
        }
        Ok(())
    }
}

impl Drop for ScreenServer {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

/// Start a loopback server consuming the caller's bounded state input.
/// The consumed token is replaced by a Secret-wrapped SHA-256 digest.
///
/// # Errors
/// Returns fixed diagnostics for invalid input or a failed bind, with no fallback.
pub async fn serve(
    bind: SocketAddr,
    pairing_token: Secret<String>,
    states: mpsc::Receiver<UiState>,
) -> Result<ScreenServer, ScreenServerError> {
    if !bind.ip().is_loopback() {
        return Err(ScreenServerError::InvalidBind);
    }
    if pairing_token.expose().is_empty() {
        return Err(ScreenServerError::InvalidToken);
    }
    let core = Arc::new(Mutex::new(Core::new(&pairing_token)));
    drop(pairing_token);
    let listener = TcpListener::bind(bind).await.map_err(|_| ScreenServerError::Bind)?;
    let bound_addr = listener.local_addr().map_err(|_| ScreenServerError::Bind)?;
    let (actions_tx, actions) = mpsc::channel(32);
    let (stop, stop_rx) = watch::channel(false);
    let app = screen_http::router(core.clone(), actions_tx, stop_rx.clone(), bound_addr);
    let listener_task = tokio::spawn(screen_listener::run(listener, app, stop_rx));
    let state_task = tokio::spawn(screen_core::pump(core.clone(), states, stop.clone()));
    Ok(ScreenServer {
        bound_addr,
        actions,
        core,
        stop,
        tasks: vec![state_task, listener_task],
    })
}

pub(crate) async fn cancelled(mut signal: watch::Receiver<bool>) {
    if !*signal.borrow() {
        let _ = signal.wait_for(|value| *value).await;
    }
}
