//! One pending account operation for the lifetime of paired Settings.
use crate::{
    Error, bridge_codex::HostOptions, bridge_mcp::bridge_presentation::Presentation, bridge_operator::screen_account,
    bridge_trace::Trace,
};
use echo_config::LiveMode;
use echo_screen_contract::UiState;
use echo_screen_contract::settings_v2::{
    OpenAiAccountV2, OpenAiApiKeyStatusV2, OpenAiLinkStatusV2 as LinkStatus, PortosActionV2 as Action,
    PortosChallengeV2, PortosOperationStartedV2, PortosOperationStateV2 as OperationState, PortosOperationV2,
    SettingsErrorV2, SettingsVersionV2,
};
use echo_screen_server::SettingsReply;
use echo_signin::{OperatorCommand, signin_codex::AccountError};
use std::{cell::Cell, future::Future, pin::Pin, time::SystemTime};
use tokio::sync::watch;

struct State {
    snapshot: OpenAiAccountV2,
    queued: Option<Action>,
    /// `api_key` mode has no sign-in to start or end from Settings.
    api_key: bool,
    /// The realtime voices the admitted app server listed, once it has.
    voices: Option<Vec<String>>,
    recovery: Option<Recovery>,
    attempt: u64,
}

struct Recovery {
    message: &'static str,
    checking: bool,
}

const RELINK: &str = "Relink to sign in to ChatGPT.";

#[derive(Clone)]
pub(crate) struct Account(watch::Sender<State>, Presentation, Trace);

tokio::task_local! {
    static CURRENT: Account;
}

impl Account {
    pub fn new(mode: LiveMode, presentation: Presentation, trace: Trace) -> Self {
        Self(
            watch::channel(State {
                snapshot: OpenAiAccountV2 {
                    version: SettingsVersionV2,
                    // Both modes observe the admitting account read. In
                    // `api_key` mode it reports whether the app server holds the key.
                    link_status: LinkStatus::Checking,
                    operation_state: OperationState::Idle,
                    challenge: None,
                    api_key: None,
                },
                queued: None,
                api_key: mode == LiveMode::ApiKey,
                voices: None,
                recovery: None,
                attempt: 0,
            })
            .0,
            presentation,
            trace,
        )
    }

    pub fn voices(&self) -> Option<Vec<String>> {
        self.0.borrow().voices.clone()
    }

    /// `api_key` comes from the settings host, which owns the configured file.
    pub fn snapshot(&self, api_key: Option<OpenAiApiKeyStatusV2>) -> SettingsReply {
        let mut snapshot = self.0.borrow().snapshot.clone();
        snapshot.api_key = api_key;
        SettingsReply::OpenAiSnapshot(snapshot)
    }

    pub fn start(&self, request: PortosOperationV2) -> SettingsReply {
        let action = match request {
            PortosOperationV2::Link { .. } => Action::Link,
            PortosOperationV2::Unlink { .. } => Action::Unlink,
        };
        if self.enqueue(action, false) {
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

    pub(crate) fn relink_start(&self) -> bool {
        self.enqueue(Action::Link, true)
    }

    fn enqueue(&self, action: Action, recovery_only: bool) -> bool {
        let (required, active) = match action {
            Action::Link => (LinkStatus::LinkRequired, OperationState::Linking),
            Action::Unlink => (LinkStatus::Linked, OperationState::Unlinking),
        };
        self.0.send_if_modified(|state| {
            if state.api_key
                || state.snapshot.link_status != required
                || (recovery_only && state.recovery.is_none())
                || state.recovery.as_ref().is_some_and(|recovery| recovery.checking)
                || matches!(
                    state.snapshot.operation_state,
                    OperationState::Linking | OperationState::Unlinking
                )
            {
                return false;
            }
            let Some(attempt) = state.attempt.checked_add(1) else {
                return false;
            };
            state.attempt = attempt;
            state.snapshot.operation_state = active;
            state.queued = Some(action);
            let _ = self.project(state);
            true
        })
    }

    /// Update the latch after the voice account is rejected or admitted.
    pub(crate) fn relink(&self, required: bool) -> Result<(), Error> {
        let mut result = Ok(());
        self.0.send_if_modified(|state| {
            if required {
                state.snapshot.link_status = LinkStatus::LinkRequired;
                let recovery = state.recovery.get_or_insert(Recovery {
                    message: RELINK,
                    checking: false,
                });
                if recovery.checking {
                    recovery.message = "ChatGPT still needs sign-in. Relink to try again.";
                    recovery.checking = false;
                }
            } else if state.recovery.take().is_none() {
                return false;
            }
            result = self.project(state);
            true
        });
        result
    }

    fn project(&self, state: &State) -> Result<(), Error> {
        self.1.openai(state.recovery.as_ref().map(|recovery| {
            if let Some(challenge) = &state.snapshot.challenge {
                UiState::OpenAiSignin {
                    attempt_id: format!("attempt-{}", state.attempt),
                    user_code: challenge.user_code.clone(),
                    verification_uri: challenge.verification_uri.clone(),
                    expires_at_unix: challenge.expires_at_unix,
                    muted: false,
                }
            } else {
                let linking = state.snapshot.operation_state == OperationState::Linking;
                let message = if linking {
                    "Getting a ChatGPT sign-in code…"
                } else if recovery.checking {
                    "Checking ChatGPT sign-in…"
                } else {
                    recovery.message
                };
                UiState::OpenAiRelink {
                    message: message.into(),
                    starting: linking || recovery.checking,
                    muted: false,
                }
            }
        }))
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
        loop {
            tokio::select! {
                biased;
                () = cancelled.as_mut() => return false,
                () = replacement_requested() => return false,
                () = self.requested() => (),
                () = std::future::ready(()), if !signed_out => return false,
            }
            let stopping = Cell::new(false);
            let readmit = self
                .operate(host, async {
                    tokio::select! {
                        () = cancelled.as_mut() => (),
                        () = replacement_requested() => (),
                    }
                    stopping.set(true);
                })
                .await;
            if stopping.get() || readmit {
                return !stopping.get();
            }
        }
    }

    async fn operate(&self, host: &HostOptions<'_>, cancelled: impl Future<Output = ()>) -> bool {
        let mut action = None;
        self.0.send_if_modified(|state| {
            action = state.queued.take();
            action.is_some()
        });
        let Some(action) = action else { return false };
        let command = match action {
            Action::Link => OperatorCommand::Signin { browser: false },
            Action::Unlink => OperatorCommand::Signout,
        };
        if action == Action::Link {
            self.event("auth.signin_started", None);
        }
        let result = screen_account(host, command, cancelled, |prompt, remaining| {
            let code = prompt.code.as_ref().ok_or(AccountError::InvalidResponse)?;
            let expires_at_unix = crate::bridge_time::now()
                .checked_add(remaining)
                .ok_or(AccountError::Output)?
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_err(|_| AccountError::Output)?
                .as_secs();
            self.0.send_modify(|state| {
                state.snapshot.challenge = Some(PortosChallengeV2 {
                    verification_uri: prompt.url.clone(),
                    user_code: code.clone(),
                    expires_at_unix,
                });
                let _ = self.project(state);
            });
            Ok(())
        })
        .await;
        self.settle(action, result)
    }

    fn settle(&self, action: Action, result: Result<(), Error>) -> bool {
        let mut readmit = true;
        self.0.send_modify(|state| {
            state.snapshot.challenge = None;
            if let Some(recovery) = &mut state.recovery {
                recovery.checking = result.is_ok();
                recovery.message = match result {
                    Ok(()) => RELINK,
                    Err(Error::Deadline) => "ChatGPT sign-in code expired. Relink for a new code.",
                    Err(Error::Authorization) => "ChatGPT sign-in was rejected. Relink to try again.",
                    Err(_) => "ChatGPT sign-in is unavailable. Relink to try again.",
                };
                readmit = recovery.checking;
            }
            state.snapshot.operation_state = match result {
                Ok(()) => {
                    state.snapshot.link_status = match action {
                        Action::Link if state.recovery.is_some() => LinkStatus::Checking,
                        Action::Link => LinkStatus::Linked,
                        Action::Unlink => LinkStatus::LinkRequired,
                    };
                    OperationState::Idle
                }
                Err(Error::Deadline) => OperationState::Expired,
                Err(_) => OperationState::Failed,
            };
            let _ = self.project(state);
        });
        if action == Action::Link {
            match result {
                Ok(()) => self.event("auth.signin_completed", None),
                Err(Error::Deadline) => self.event("auth.signin_expired", None),
                Err(Error::Authorization) => self.event("auth.signin_failed", Some("authorization")),
                Err(Error::Control) => self.event("auth.signin_failed", Some("cancelled")),
                Err(_) => self.event("auth.signin_failed", Some("unavailable")),
            }
        }
        readmit
    }

    fn event(&self, event: &str, reason: Option<&str>) {
        let fields = reason.map_or_else(|| serde_json::json!({}), |reason| serde_json::json!({"reason":reason}));
        let _ = self.2.clone().record(event, None, fields);
    }
}

pub(crate) fn owns_relink() -> bool {
    CURRENT.try_with(|account| !account.0.borrow().api_key).unwrap_or(false)
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
