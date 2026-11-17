//! Consent belongs to a currently presented request, never to model arguments.
use crate::{Error, bridge_mcp::bridge_presentation::Presentation};
use echo_screen_contract::{UiAction, UiState};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::oneshot;

static IDS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Default)]
pub(crate) struct Approvals(Arc<Mutex<Option<Pending>>>);

struct Pending {
    id: String,
    answer: oneshot::Sender<bool>,
}

impl Approvals {
    pub fn answer(&self, action: &UiAction) -> Result<(), Error> {
        let (id, accepted) = match action {
            UiAction::Approve { id } => (id, true),
            UiAction::Deny { id } => (id, false),
            _ => return Ok(()),
        };
        let mut pending = self.0.lock().map_err(|_| Error::Screen)?;
        if pending.as_ref().is_some_and(|request| &request.id == id)
            && let Some(request) = pending.take()
        {
            let _ = request.answer.send(accepted);
        }
        Ok(())
    }

    pub async fn request(&self, presentation: &Presentation) -> Result<bool, Error> {
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Error::Screen)?
            .as_nanos();
        let sequence = IDS
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| Error::Screen)?;
        let id = format!("approval-{epoch}-{sequence}");
        let (send, receive) = oneshot::channel();
        {
            let mut pending = self.0.lock().map_err(|_| Error::Screen)?;
            if pending.is_some() {
                return Err(Error::Screen);
            }
            *pending = Some(Pending {
                id: id.clone(),
                answer: send,
            });
        }
        let guard = Prompt {
            approvals: self,
            presentation,
            id: &id,
        };
        presentation.approval(UiState::Approval {
            id: id.clone(),
            prompt: "Allow test.requires_approval?".into(),
            accept_label: "Allow".into(),
            deny_label: "Deny".into(),
        })?;
        let accepted = receive.await.map_err(|_| Error::Screen)?;
        presentation.clear_approval(&id)?;
        drop(guard);
        Ok(accepted)
    }
}

struct Prompt<'a> {
    approvals: &'a Approvals,
    presentation: &'a Presentation,
    id: &'a str,
}

impl Drop for Prompt<'_> {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.approvals.0.lock()
            && pending.as_ref().is_some_and(|request| request.id == self.id)
        {
            *pending = None;
        }
        // Cancelled dispatch retires consent and restores the latest activity.
        let _ = self.presentation.clear_approval(self.id);
    }
}
