use super::{Grant, bridge_presentation::RELINK};
use echo_mcp_auth::AuthError;
use echo_screen_contract::{
    UiState,
    settings_v2::{PortosChallengeV2, PortosOperationStateV2},
};
use serde_json::json;
use std::sync::{Arc, atomic::Ordering};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    Link,
    Refresh,
    Unlink,
}

impl Operation {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "link" => Some(Self::Link),
            "refresh" => Some(Self::Refresh),
            "unlink" => Some(Self::Unlink),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::Refresh => "refresh",
            Self::Unlink => "unlink",
        }
    }

    pub fn outcome(self) -> &'static str {
        match self {
            Self::Link => "linked",
            Self::Refresh => "refreshed",
            Self::Unlink => "unlinked",
        }
    }
}

pub(super) struct Request {
    pub operation: Operation,
    pub reply: oneshot::Sender<Result<&'static str, AuthError>>,
    pub permit: OwnedSemaphorePermit,
    pub progress: mpsc::Sender<UiState>,
}

#[derive(Clone)]
pub(crate) struct Operator {
    send: mpsc::Sender<Request>,
    slot: Arc<Semaphore>,
}

pub(crate) struct OperationReceipt {
    pub result: oneshot::Receiver<Result<&'static str, AuthError>>,
    pub progress: mpsc::Receiver<UiState>,
}

pub(crate) enum StartError {
    Busy,
    Closed,
}

#[derive(Default)]
pub(super) enum ScreenOperation {
    #[default]
    Idle,
    Pending {
        operation: Operation,
        receipt: OperationReceipt,
        challenge: Option<PortosChallengeV2>,
    },
    Failed(PortosOperationStateV2),
}

impl Grant {
    pub(crate) fn start_screen_operation(&self, operation: Operation) -> Result<(), StartError> {
        let mut screen = self.screen_operation.lock().map_err(|_| StartError::Closed)?;
        let receipt = self.operator.start(operation)?;
        *screen = ScreenOperation::Pending {
            operation,
            receipt,
            challenge: None,
        };
        Ok(())
    }

    pub(crate) fn screen_operation(&self) -> (PortosOperationStateV2, Option<PortosChallengeV2>) {
        let Ok(mut screen) = self.screen_operation.lock() else {
            return (PortosOperationStateV2::Failed, None);
        };
        screen.update(self.now());
        match &*screen {
            ScreenOperation::Idle => (PortosOperationStateV2::Idle, None),
            ScreenOperation::Failed(state) => (*state, None),
            ScreenOperation::Pending {
                operation, challenge, ..
            } => {
                let state = if *operation == Operation::Unlink {
                    PortosOperationStateV2::Unlinking
                } else {
                    PortosOperationStateV2::Linking
                };
                (state, challenge.clone())
            }
        }
    }

    pub(crate) fn screen_operation_active(&self) -> bool {
        matches!(
            self.screen_operation().0,
            PortosOperationStateV2::Linking | PortosOperationStateV2::Unlinking
        )
    }
}

impl ScreenOperation {
    fn update(&mut self, now: u64) {
        let Self::Pending { receipt, challenge, .. } = self else {
            return;
        };
        if let Ok(UiState::LinkCode {
            verification_uri,
            user_code,
            expires_at_unix,
        }) = receipt.progress.try_recv()
        {
            *challenge = Some(PortosChallengeV2 {
                verification_uri,
                user_code,
                expires_at_unix,
            });
        }
        if challenge.as_ref().is_some_and(|code| now >= code.expires_at_unix) {
            *challenge = None;
        }
        *self = match receipt.result.try_recv() {
            Ok(Ok(_)) => Self::Idle,
            Ok(Err(AuthError::Expired)) => Self::Failed(PortosOperationStateV2::Expired),
            Ok(Err(AuthError::AccessDenied)) => Self::Failed(PortosOperationStateV2::Denied),
            Err(oneshot::error::TryRecvError::Empty) => return,
            _ => Self::Failed(PortosOperationStateV2::Failed),
        };
    }
}

impl Operator {
    pub(super) fn new(send: mpsc::Sender<Request>) -> Self {
        Self {
            send,
            slot: Arc::new(Semaphore::new(1)),
        }
    }

    pub(crate) fn start(&self, operation: Operation) -> Result<OperationReceipt, StartError> {
        let permit = self.slot.clone().try_acquire_owned().map_err(|_| StartError::Busy)?;
        let (reply, result) = oneshot::channel();
        let (progress, updates) = mpsc::channel(1);
        self.send
            .try_send(Request {
                operation,
                reply,
                permit,
                progress,
            })
            .map_err(|_| StartError::Closed)?;
        Ok(OperationReceipt {
            result,
            progress: updates,
        })
    }
}

pub(super) async fn serve(grant: Arc<Grant>, mut receive: mpsc::Receiver<Request>) {
    while let Some(Request {
        operation,
        mut reply,
        permit,
        progress,
    }) = receive.recv().await
    {
        let result = tokio::select! {
            biased;
            () = reply.closed() => { grant.reject(RELINK); None },
            result = execute(&grant, operation, progress) => Some(result),
        };
        if let Some(result) = result {
            let _ = reply.send(result);
        }
        drop(permit);
    }
}

async fn execute(
    grant: &Grant,
    operation: Operation,
    progress: mpsc::Sender<UiState>,
) -> Result<&'static str, AuthError> {
    let id = grant.next_operation()?;
    if operation == Operation::Link {
        grant.event("mcp.link.started", json!({"operation_id":id}))?;
    }
    let result = match operation {
        Operation::Link => link(grant, progress).await,
        Operation::Refresh if grant.blocked() => Err(AuthError::RelinkRequired),
        Operation::Refresh => grant.client.force_refresh().await.map(|_| ()),
        Operation::Unlink => {
            grant.reject(RELINK);
            grant.client.revoke().await
        }
    };
    let result = result.and_then(|()| grant.check().map_err(|_| AuthError::Storage));
    if let Err(error) = &result {
        if *error == AuthError::RelinkRequired {
            grant.reject(RELINK);
        } else {
            grant.reject(&error.to_string());
        }
    }
    result?;
    let event = match operation {
        Operation::Link => "mcp.link.approved",
        Operation::Refresh => "mcp.refresh.committed",
        Operation::Unlink => "mcp.unlink.completed",
    };
    grant.event(event, json!({"operation_id":id}))?;
    Ok(operation.outcome())
}

async fn link(grant: &Grant, progress: mpsc::Sender<UiState>) -> Result<(), AuthError> {
    grant.rejected.store(true, Ordering::Release);
    let link = grant.client.begin_link().await?;
    let epoch = grant.now();
    let state = UiState::LinkCode {
        user_code: link.user_code.clone(),
        verification_uri: link.verification_uri.clone(),
        expires_at_unix: epoch.checked_add(link.expires_in).ok_or(AuthError::Protocol)?,
    };
    grant
        .presentation
        .overlay(Some(state.clone()))
        .map_err(|_| AuthError::Storage)?;
    progress.try_send(state).map_err(|_| AuthError::Storage)?;
    grant.client.await_approval(link).await?;
    grant.reloaded.store(false, Ordering::Release);
    grant.presentation.overlay(None).map_err(|_| AuthError::Storage)?;
    grant.reset_scope_status()?;
    grant.rejected.store(false, Ordering::Release);
    Ok(())
}
