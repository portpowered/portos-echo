//! Authenticated, bounded loopback screen transport. LAN exposure requires W3 TLS.
mod packs;
mod screen_assets;
mod screen_core;
mod screen_http;
mod screen_listener;
mod screen_settings;
mod screen_url;

pub use screen_settings::{PromptKind, SettingsOperation, SettingsReply, SettingsRequest, settings_channel};
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
    publication_requests: mpsc::Sender<tokio::sync::oneshot::Sender<()>>,
    core: Arc<Mutex<Core>>,
    stop: watch::Sender<bool>,
    drain: watch::Sender<Option<tokio::time::Instant>>,
    listener: Option<JoinHandle<usize>>,
    tasks: Vec<JoinHandle<()>>,
    downloads_started: bool,
    pack_files: Arc<packs::Files>,
}

/// Safe completion for one configured background download. No input URL or path.
pub struct PackInstallResult {
    pub installed: Option<String>,
    pub reason: Option<String>,
}

impl ScreenServer {
    /// Install configured pins serially outside the voice path. Listener lifetime
    /// owns cancellation and joins synchronous validation before shutdown returns.
    /// Failures retain existing installed directories and never stop the server.
    pub fn install_configured_packs(
        &mut self,
        home: std::path::PathBuf,
        ca: Option<std::path::PathBuf>,
        pins: Vec<(String, String)>,
    ) -> mpsc::Receiver<PackInstallResult> {
        let (send, receive) = mpsc::channel(4);
        if self.downloads_started {
            return receive;
        }
        self.downloads_started = true;
        let _ = self.pack_files.root.set(home.join("screens"));
        self.tasks.push(tokio::spawn(packs::start(
            packs::Downloads { home, ca, pins },
            self.core.clone(),
            self.stop.subscribe(),
            self.drain.subscribe(),
            send,
        )));
        receive
    }

    /// Acknowledge states admitted before this call through the existing pump.
    /// Success commits the current snapshot, not delivery to existing clients.
    ///
    /// # Errors
    /// Returns `Task` on deadline, shutdown or a closed pump.
    pub async fn flush(&self, deadline: tokio::time::Instant) -> Result<(), ScreenServerError> {
        let (send, receive) = tokio::sync::oneshot::channel();
        tokio::time::timeout_at(deadline, async {
            if *self.stop.borrow() {
                return Err(ScreenServerError::Task);
            }
            self.publication_requests
                .send(send)
                .await
                .map_err(|_| ScreenServerError::Task)?;
            receive.await.map_err(|_| ScreenServerError::Task)
        })
        .await
        .map_err(|_| ScreenServerError::Task)?
    }

    /// Close active streams and bar future authorization before returning.
    /// Bytes already delivered to the operating system cannot be recalled.
    ///
    /// # Errors
    /// Reserved for task lifecycle failures.
    pub async fn revoke_pairing(&self) -> Result<(), ScreenServerError> {
        self.core.lock().await.revoke();
        Ok(())
    }

    /// Drain already-queued frames and join owned tasks. One 250 ms deadline
    /// bounds all connections together. Returns the number dropped at that bound.
    /// This delivery guarantee is separate from snapshot admission by `flush`.
    /// Dropping the server cancels immediately; revocation never drains.
    ///
    /// # Errors
    /// Returns `Task` when an owned task failed.
    pub async fn shutdown_gracefully(&mut self) -> Result<usize, ScreenServerError> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(250);
        self.core.lock().await.drain();
        self.drain.send_if_modified(|value| {
            if value.is_some() {
                return false;
            }
            *value = Some(deadline);
            true
        });
        self.join_tasks().await
    }

    /// Cancel immediately and join owned tasks without draining frames.
    ///
    /// # Errors
    /// Returns `Task` when an owned task failed.
    pub async fn shutdown(&mut self) -> Result<(), ScreenServerError> {
        self.stop.send_replace(true);
        self.join_tasks().await.map(|_| ())
    }

    async fn join_tasks(&mut self) -> Result<usize, ScreenServerError> {
        let dropped = if let Some(listener) = &mut self.listener {
            listener.await.map_err(|_| ScreenServerError::Task)?
        } else {
            0
        };
        self.listener.take();
        self.stop.send_replace(true);
        for task in self.tasks.drain(..) {
            task.await.map_err(|_| ScreenServerError::Task)?;
        }
        Ok(dropped)
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
    serve_with_settings(bind, pairing_token, states, None).await
}

/// Start the same server with an optional home-backed settings inbox.
/// Existing callers without an inbox retain identical assets and screen frames.
///
/// # Errors
/// Returns the same fixed input/bind diagnostics as `serve`.
pub async fn serve_with_settings(
    bind: SocketAddr,
    pairing_token: Secret<String>,
    states: mpsc::Receiver<UiState>,
    settings: Option<mpsc::Sender<SettingsRequest>>,
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
    let (drain, drain_rx) = watch::channel(None);
    let pack_files = Arc::new(packs::Files::default());
    let app = screen_http::router(
        core.clone(),
        actions_tx,
        stop_rx.clone(),
        drain_rx.clone(),
        bound_addr,
        settings,
        pack_files.clone(),
    );
    let listener_task = tokio::spawn(screen_listener::run(listener, app, stop_rx, drain_rx));
    let (publication_requests, requests) = mpsc::channel(32);
    let state_task = tokio::spawn(screen_core::pump(core.clone(), states, requests, stop.clone()));
    Ok(ScreenServer {
        bound_addr,
        actions,
        publication_requests,
        core,
        stop,
        drain,
        listener: Some(listener_task),
        tasks: vec![state_task],
        downloads_started: false,
        pack_files,
    })
}

pub(crate) async fn cancelled(mut signal: watch::Receiver<bool>) {
    if !*signal.borrow() {
        let _ = signal.wait_for(|value| *value).await;
    }
}

pub(crate) async fn draining(mut signal: watch::Receiver<Option<tokio::time::Instant>>) {
    let _ = signal.wait_for(Option::is_some).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{future::Future, time::Duration};
    use tokio::time::Instant;

    fn offline(capacity: usize) -> (ScreenServer, mpsc::Receiver<tokio::sync::oneshot::Sender<()>>) {
        let (publication_requests, requests) = mpsc::channel(capacity);
        (
            ScreenServer {
                bound_addr: ([127, 0, 0, 1], 0).into(),
                actions: mpsc::channel(1).1,
                publication_requests,
                core: Arc::new(Mutex::new(Core::new(&Secret::new("synthetic-screen-pair".into())))),
                stop: watch::channel(false).0,
                drain: watch::channel(None).0,
                listener: None,
                tasks: vec![],
                downloads_started: false,
                pack_files: Arc::default(),
            },
            requests,
        )
    }

    #[tokio::test]
    async fn flush_success_requires_a_receipt() {
        let (server, mut requests) = offline(1);
        let flush = server.flush(Instant::now() + Duration::from_secs(5));
        tokio::pin!(flush);
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(flush.as_mut().poll(cx).is_pending())).await);
        requests.recv().await.unwrap().send(()).unwrap();
        assert!(flush.await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn flush_uses_one_deadline_for_admission_and_receipt() {
        let (server, mut requests) = offline(1);
        server
            .publication_requests
            .try_send(tokio::sync::oneshot::channel().0)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let flush = server.flush(deadline);
        tokio::pin!(flush);
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(flush.as_mut().poll(cx).is_pending())).await);
        tokio::time::advance(Duration::from_secs(4)).await;
        drop(requests.recv().await.unwrap());
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(flush.as_mut().poll(cx).is_pending())).await);
        let _held = requests.recv().await.unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(matches!(flush.await, Err(ScreenServerError::Task)));
        assert_eq!(Instant::now(), deadline);
    }

    #[tokio::test(start_paused = true)]
    async fn flush_handles_saturation_closure_stop_and_cancelled_receipt() {
        let (server, mut requests) = offline(1);
        server
            .publication_requests
            .try_send(tokio::sync::oneshot::channel().0)
            .unwrap();
        assert!(server.flush(Instant::now() + Duration::from_secs(5)).await.is_err());
        drop(requests.recv().await.unwrap());
        let flush = server.flush(Instant::now() + Duration::from_secs(5));
        tokio::pin!(flush);
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(flush.as_mut().poll(cx).is_pending())).await);
        drop(requests.recv().await.unwrap());
        assert!(flush.await.is_err());
        drop(requests);
        assert!(server.flush(Instant::now() + Duration::from_secs(5)).await.is_err());
        server.stop.send_replace(true);
        assert!(server.flush(Instant::now() + Duration::from_secs(5)).await.is_err());
    }
}
