//! One pending account operation for the lifetime of paired Settings.
use crate::{Error, bridge_codex::HostOptions, bridge_operator::screen_account};
use echo_config::LiveMode;
use echo_screen_contract::settings_v2::{
    OpenAiAccountV2, OpenAiLinkStatusV2 as LinkStatus, PortosActionV2 as Action, PortosChallengeV2,
    PortosOperationStartedV2, PortosOperationStateV2 as OperationState, PortosOperationV2, SettingsErrorV2,
    SettingsVersionV2,
};
use echo_screen_server::SettingsReply;
use echo_signin::{OperatorCommand, signin_codex::AccountError};
use std::{cell::Cell, future::Future, pin::Pin, time::SystemTime};
use tokio::sync::watch;

struct State {
    snapshot: OpenAiAccountV2,
    queued: Option<Action>,
    /// The realtime voices the admitted app server listed, once it has.
    voices: Option<Vec<String>>,
}

#[derive(Clone)]
pub(crate) struct Account(watch::Sender<State>);

tokio::task_local! {
    static CURRENT: Account;
}

impl Account {
    pub fn new(mode: LiveMode) -> Self {
        Self(
            watch::channel(State {
                snapshot: OpenAiAccountV2 {
                    version: SettingsVersionV2,
                    link_status: if mode == LiveMode::Chatgpt {
                        LinkStatus::Checking
                    } else {
                        LinkStatus::Unavailable
                    },
                    operation_state: OperationState::Idle,
                    challenge: None,
                },
                queued: None,
                voices: None,
            })
            .0,
        )
    }

    pub fn voices(&self) -> Option<Vec<String>> {
        self.0.borrow().voices.clone()
    }

    pub fn snapshot(&self) -> SettingsReply {
        SettingsReply::OpenAiSnapshot(self.0.borrow().snapshot.clone())
    }

    pub fn start(&self, request: PortosOperationV2) -> SettingsReply {
        let (action, required, active) = match request {
            PortosOperationV2::Link { .. } => (Action::Link, LinkStatus::LinkRequired, OperationState::Linking),
            PortosOperationV2::Unlink { .. } => (Action::Unlink, LinkStatus::Linked, OperationState::Unlinking),
        };
        let accepted = self.0.send_if_modified(|state| {
            if state.snapshot.link_status != required
                || matches!(
                    state.snapshot.operation_state,
                    OperationState::Linking | OperationState::Unlinking
                )
            {
                return false;
            }
            state.snapshot.operation_state = active;
            state.queued = Some(action);
            true
        });
        if accepted {
            SettingsReply::AccountStarted(PortosOperationStartedV2 {
                version: SettingsVersionV2,
                operation: action,
            })
        } else {
            SettingsReply::AccountConflict(SettingsErrorV2 {
                version: SettingsVersionV2,
                message: "OpenAI account operation is unavailable in the current state".into(),
            })
        }
    }

    pub async fn scope<T>(&self, future: impl Future<Output = T>) -> T {
        CURRENT.scope(self.clone(), future).await
    }

    async fn requested(&self) {
        let _ = self.0.subscribe().wait_for(|state| state.queued.is_some()).await;
    }

    /// Wait on the signed-out screen or service a queued request after the voice owner has stopped.
    pub async fn handoff(
        &self,
        host: &HostOptions<'_>,
        mut cancelled: Pin<&mut impl Future<Output = ()>>,
        signed_out: bool,
    ) -> bool {
        use super::settings_restart::replacement_requested;
        tokio::select! {
            biased;
            () = cancelled.as_mut() => return false,
            () = replacement_requested() => return false,
            () = self.requested() => (),
            () = std::future::ready(()), if !signed_out => return false,
        }
        let stopping = Cell::new(false);
        self.operate(host, async {
            tokio::select! {
                () = cancelled => (),
                () = replacement_requested() => (),
            }
            stopping.set(true);
        })
        .await;
        !stopping.get()
    }

    #[allow(clippy::disallowed_methods, reason = "Existing host time")]
    async fn operate(&self, host: &HostOptions<'_>, cancelled: impl Future<Output = ()>) {
        let mut action = None;
        self.0.send_if_modified(|state| {
            action = state.queued.take();
            action.is_some()
        });
        let Some(action) = action else { return };
        let command = match action {
            Action::Link => OperatorCommand::Signin { browser: false },
            Action::Unlink => OperatorCommand::Signout,
        };
        let result = screen_account(host, command, cancelled, |prompt, remaining| {
            let code = prompt.code.as_ref().ok_or(AccountError::InvalidResponse)?;
            let expires_at_unix = (SystemTime::now() + remaining)
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_err(|_| AccountError::Output)?
                .as_secs();
            self.0.send_modify(|state| {
                state.snapshot.challenge = Some(PortosChallengeV2 {
                    verification_uri: prompt.url.clone(),
                    user_code: code.clone(),
                    expires_at_unix,
                });
            });
            Ok(())
        })
        .await;
        self.0.send_modify(|state| {
            state.snapshot.challenge = None;
            state.snapshot.operation_state = match result {
                Ok(()) => {
                    state.snapshot.link_status = match action {
                        Action::Link => LinkStatus::Linked,
                        Action::Unlink => LinkStatus::LinkRequired,
                    };
                    OperationState::Idle
                }
                Err(Error::Deadline) => OperationState::Expired,
                Err(_) => OperationState::Failed,
            };
        });
    }
}

/// Status comes from the same account read that admits the voice generation.
pub(crate) fn observed(result: Result<(), AccountError>) {
    let status = match result {
        Ok(()) => LinkStatus::Linked,
        Err(AccountError::Authorization) => LinkStatus::LinkRequired,
        Err(_) => return,
    };
    let _ = CURRENT.try_with(|account| {
        account.0.send_if_modified(|state| {
            if matches!(state.snapshot.link_status, LinkStatus::Unavailable) || state.snapshot.link_status == status {
                return false;
            }
            state.snapshot.link_status = status;
            true
        });
    });
}

/// The voice list comes from the same process generation that serves the calls.
/// A generation that could not list keeps the last list read.
pub(crate) fn voices_observed(voices: Option<&[String]>) {
    let Some(voices) = voices else { return };
    let _ = CURRENT.try_with(|account| {
        account.0.send_if_modified(|state| {
            let changed = state.voices.as_deref() != Some(voices);
            if changed {
                state.voices = Some(voices.to_vec());
            }
            changed
        });
    });
}

/// Share the settled voice boundary with settings restart without requesting exec.
pub(crate) async fn requested() {
    if let Ok(account) = CURRENT.try_with(Clone::clone) {
        account.requested().await;
    } else {
        std::future::pending::<()>().await;
    }
}
