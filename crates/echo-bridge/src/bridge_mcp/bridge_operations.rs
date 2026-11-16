use super::{Grant, bridge_presentation::RELINK};
use echo_mcp_auth::AuthError;
use echo_screen_contract::UiState;
use serde_json::json;
use std::sync::{Arc, atomic::Ordering};
use tokio::sync::{OwnedSemaphorePermit, mpsc, oneshot};

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

pub(crate) struct Request {
    pub operation: Operation,
    pub reply: oneshot::Sender<Result<&'static str, AuthError>>,
    pub permit: OwnedSemaphorePermit,
}

pub(super) async fn serve(grant: Arc<Grant>, mut receive: mpsc::Receiver<Request>) {
    while let Some(Request {
        operation,
        mut reply,
        permit,
    }) = receive.recv().await
    {
        let result = tokio::select! {
            biased;
            () = reply.closed() => { grant.reject(RELINK); None },
            result = execute(&grant, operation) => Some(result),
        };
        if let Some(result) = result {
            let _ = reply.send(result);
        }
        drop(permit);
    }
}

async fn execute(grant: &Grant, operation: Operation) -> Result<&'static str, AuthError> {
    let id = grant.next_operation()?;
    if operation == Operation::Link {
        grant.event("mcp.link.started", json!({"operation_id":id}))?;
    }
    let result = match operation {
        Operation::Link => link(grant).await,
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

async fn link(grant: &Grant) -> Result<(), AuthError> {
    grant.rejected.store(true, Ordering::Release);
    let link = grant.client.begin_link().await?;
    let epoch = grant.now();
    grant
        .presentation
        .overlay(Some(UiState::LinkCode {
            user_code: link.user_code.clone(),
            verification_uri: link.verification_uri.clone(),
            expires_at_unix: epoch.checked_add(link.expires_in).ok_or(AuthError::Protocol)?,
        }))
        .map_err(|_| AuthError::Storage)?;
    grant.client.await_approval(link).await?;
    grant.reloaded.store(false, Ordering::Release);
    grant.presentation.overlay(None).map_err(|_| AuthError::Storage)?;
    grant.rejected.store(false, Ordering::Release);
    Ok(())
}
