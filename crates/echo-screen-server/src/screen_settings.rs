//! Bounded typed dispatch. The bridge owns validation, persistence and restart.
use crate::{cancelled, screen_core::Core, screen_http::authorized};
use axum::{
    Json,
    http::{StatusCode, Uri},
    response::{IntoResponse, Response},
};
use echo_screen_contract::settings_v1::{
    PromptSaveV1, SaveResultV1, SettingsErrorV1, SettingsPatchV1, SettingsSnapshotV1,
};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, oneshot, watch};

/// Fixed prompt selector; never a user-controlled filesystem path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    Voice,
    Runner,
}

/// Decoded input, with no arbitrary key/path or executable surface.
pub enum SettingsOperation {
    Snapshot,
    PackCatalog(crate::PackCatalog),
    PackSelection(crate::PackCatalog, String),
    Save(SettingsPatchV1),
    Prompt(PromptKind, PromptSaveV1),
}

/// Host diagnostics must contain only trusted field names and fixed categories.
pub enum SettingsReply {
    Snapshot(SettingsSnapshotV1),
    Packs(crate::PackCatalog),
    InvalidPackSelection,
    Saved(SaveResultV1),
    /// Durable pack-only save, resolved against the same validated catalog.
    PackSaved(SaveResultV1, crate::PackCatalog, bool),
    Invalid(SettingsErrorV1),
    Persistence(SettingsErrorV1),
}

impl SettingsReply {
    fn into_response(self) -> Response {
        match self {
            Self::Packs(value) => Json(value).into_response(),
            Self::InvalidPackSelection => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({"version":1,"reason":"invalid_selection"})),
            )
                .into_response(),
            Self::Snapshot(value) => Json(value).into_response(),
            Self::Saved(value) | Self::PackSaved(value, _, _) => Json(value).into_response(),
            Self::Invalid(value) => (StatusCode::UNPROCESSABLE_ENTITY, Json(value)).into_response(),
            Self::Persistence(value) => (StatusCode::INTERNAL_SERVER_ERROR, Json(value)).into_response(),
        }
    }
}

/// One pending request. No operation is exposed before final commit admission.
/// Dropping the HTTP future cancels requests not yet admitted by `commit`.
pub struct SettingsRequest {
    operation: SettingsOperation,
    uri: Option<Uri>,
    installed: bool,
    core: Arc<Mutex<Core>>,
    stop: watch::Receiver<bool>,
    drain: watch::Receiver<Option<tokio::time::Instant>>,
    reply: oneshot::Sender<Response>,
}

impl SettingsRequest {
    /// Run one synchronous transaction while revocation and shutdown admission
    /// are serialized. The host must not schedule work outside this closure.
    /// A failed precommit check invokes no host code; there is no queued retry.
    pub async fn commit(self, transaction: impl FnOnce(SettingsOperation) -> SettingsReply) {
        let mut core = self.core.lock().await;
        if self.reply.is_closed() {
            return;
        }
        let response = if self.uri.as_ref().is_some_and(|uri| !authorized(&core, uri)) {
            StatusCode::UNAUTHORIZED.into_response()
        } else if *self.stop.borrow()
            || self.drain.borrow().is_some()
            || !core.accepting()
            || (matches!(&self.operation, SettingsOperation::PackSelection(..))
                && core.pack_revision == 9_007_199_254_740_991)
        {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        } else {
            let mut reply = transaction(self.operation);
            match &mut reply {
                SettingsReply::PackSaved(_, catalog, true) => {
                    core.pack_reload(catalog.active());
                }
                SettingsReply::Packs(catalog) => {
                    if self.installed {
                        core.pack_reload(catalog.active());
                    }
                    catalog.set_revision(core.pack_revision);
                }
                _ => {}
            }
            reply.into_response()
        };
        let _ = self.reply.send(response);
    }
}

/// A fixed-size inbox keeps a slow host from creating an unbounded work queue.
#[must_use]
pub fn settings_channel() -> (mpsc::Sender<SettingsRequest>, mpsc::Receiver<SettingsRequest>) {
    mpsc::channel(32)
}

#[derive(Clone)]
pub(crate) struct SettingsDispatch {
    pub core: Arc<Mutex<Core>>,
    pub stop: watch::Receiver<bool>,
    pub drain: watch::Receiver<Option<tokio::time::Instant>>,
    pub host: Option<mpsc::Sender<SettingsRequest>>,
}

impl SettingsDispatch {
    pub async fn send(self, uri: Uri, operation: SettingsOperation) -> Response {
        self.dispatch(Some(uri), operation, false).await
    }

    // Only the lifetime-owned installer can create this notification. It reads
    // the accepted selection through the existing host, never through TOML or a
    // retained browser credential. Commit still serializes revocation/shutdown.
    pub(crate) async fn installed(self, catalog: crate::PackCatalog) {
        let _ = self.dispatch(None, SettingsOperation::PackCatalog(catalog), true).await;
    }

    async fn dispatch(self, uri: Option<Uri>, operation: SettingsOperation, installed: bool) -> Response {
        let core = self.core.lock().await;
        if uri.as_ref().is_some_and(|uri| !authorized(&core, uri)) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        if *self.stop.borrow() || self.drain.borrow().is_some() || !core.accepting() {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        let Some(host) = self.host else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let (reply, response) = oneshot::channel();
        let request = SettingsRequest {
            operation,
            uri,
            installed,
            core: self.core.clone(),
            stop: self.stop.clone(),
            drain: self.drain.clone(),
            reply,
        };
        drop(core);
        if installed {
            tokio::select! {
                biased;
                () = cancelled(self.stop.clone()) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                () = crate::draining(self.drain.clone()) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                result = host.send(request) => if result.is_err() { return StatusCode::SERVICE_UNAVAILABLE.into_response(); },
            }
        } else if host.try_send(request).is_err() {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        tokio::select! {
            biased;
            () = cancelled(self.stop) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            () = crate::draining(self.drain) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            response = response => response.unwrap_or_else(|_| StatusCode::SERVICE_UNAVAILABLE.into_response()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_contracts::secret::Secret;
    use std::{future::Future, task::Poll};

    const URI: &str = "/settings/v1?token=synthetic-screen-pair";

    struct Fixture {
        dispatch: SettingsDispatch,
        inbox: mpsc::Receiver<SettingsRequest>,
        stop: watch::Sender<bool>,
        drain: watch::Sender<Option<tokio::time::Instant>>,
    }

    fn fixture() -> Fixture {
        let (host, inbox) = mpsc::channel(1);
        let (stop, stop_rx) = watch::channel(false);
        let (drain, drain_rx) = watch::channel(None);
        Fixture {
            dispatch: SettingsDispatch {
                core: Arc::new(Mutex::new(Core::new(&Secret::new("synthetic-screen-pair".into())))),
                stop: stop_rx,
                drain: drain_rx,
                host: Some(host),
            },
            inbox,
            stop,
            drain,
        }
    }

    #[tokio::test]
    async fn settings_cancelled_before_commit_never_invokes_host() {
        let mut fixture = fixture();
        let request = {
            let sending = fixture
                .dispatch
                .clone()
                .send(URI.parse().unwrap(), SettingsOperation::Snapshot);
            tokio::pin!(sending);
            assert!(std::future::poll_fn(|cx| Poll::Ready(sending.as_mut().poll(cx).is_pending())).await);
            fixture.inbox.recv().await.unwrap()
        };
        request.commit(|_| unreachable!("cancelled work ran")).await;
    }

    #[tokio::test]
    async fn settings_saturation_has_no_hidden_retry_or_extra_dispatch() {
        let mut fixture = fixture();
        let first = fixture
            .dispatch
            .clone()
            .send(URI.parse().unwrap(), SettingsOperation::Snapshot);
        tokio::pin!(first);
        assert!(std::future::poll_fn(|cx| Poll::Ready(first.as_mut().poll(cx).is_pending())).await);
        let refused = fixture
            .dispatch
            .clone()
            .send(URI.parse().unwrap(), SettingsOperation::Snapshot)
            .await;
        assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
        let held = fixture.inbox.recv().await.unwrap();
        assert!(matches!(
            fixture.inbox.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        drop(held);
        assert_eq!(first.await.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn settings_stop_and_drain_cancel_queued_commit() {
        for stop in [true, false] {
            let mut fixture = fixture();
            let sending = fixture
                .dispatch
                .clone()
                .send(URI.parse().unwrap(), SettingsOperation::Snapshot);
            tokio::pin!(sending);
            assert!(std::future::poll_fn(|cx| Poll::Ready(sending.as_mut().poll(cx).is_pending())).await);
            let request = fixture.inbox.recv().await.unwrap();
            if stop {
                fixture.stop.send_replace(true);
            } else {
                fixture.drain.send_replace(Some(tokio::time::Instant::now()));
            }
            request.commit(|_| unreachable!("stopped work ran")).await;
            assert_eq!(sending.await.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
    }

    #[tokio::test]
    async fn settings_installer_notification_rechecks_lifetime_before_host_commit() {
        for signal in 0..3 {
            let mut fixture = fixture();
            let sending = fixture
                .dispatch
                .clone()
                .dispatch(None, SettingsOperation::Snapshot, true);
            tokio::pin!(sending);
            assert!(std::future::poll_fn(|cx| Poll::Ready(sending.as_mut().poll(cx).is_pending())).await);
            let request = fixture.inbox.recv().await.unwrap();
            match signal {
                0 => {
                    fixture.stop.send_replace(true);
                }
                1 => {
                    fixture.drain.send_replace(Some(tokio::time::Instant::now()));
                }
                _ => fixture.dispatch.core.lock().await.revoke(),
            }
            request
                .commit(|_| unreachable!("cancelled installer notification reached host"))
                .await;
            assert_eq!(sending.await.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(fixture.dispatch.core.lock().await.pack_revision, 0);
        }
    }

    #[tokio::test]
    async fn settings_installer_notification_waits_for_bounded_host_capacity() {
        let mut fixture = fixture();
        let first = fixture
            .dispatch
            .clone()
            .send(URI.parse().unwrap(), SettingsOperation::Snapshot);
        tokio::pin!(first);
        assert!(std::future::poll_fn(|cx| Poll::Ready(first.as_mut().poll(cx).is_pending())).await);
        let installed = fixture
            .dispatch
            .clone()
            .dispatch(None, SettingsOperation::Snapshot, true);
        tokio::pin!(installed);
        assert!(std::future::poll_fn(|cx| Poll::Ready(installed.as_mut().poll(cx).is_pending())).await);
        fixture
            .inbox
            .recv()
            .await
            .unwrap()
            .commit(|_| SettingsReply::InvalidPackSelection)
            .await;
        assert_eq!(first.await.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(std::future::poll_fn(|cx| Poll::Ready(installed.as_mut().poll(cx).is_pending())).await);
        fixture
            .inbox
            .recv()
            .await
            .unwrap()
            .commit(|_| SettingsReply::InvalidPackSelection)
            .await;
        assert_eq!(installed.await.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(matches!(
            fixture.inbox.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }
}
