//! Writer-based operator commands, hosted later by `echo-bridge`.
//! `signin` defaults to device approval; `signin --browser` reports the PKCE
//! URL without launching a browser. Import always needs an explicit path.
//! Status reads stored metadata without refreshing. The host owns argv parsing,
//! exit codes, traces and cancellation of active calls/audio on a kill signal.
pub mod signin_codex;
mod signin_commands;
mod signin_providers;

use echo_chatgpt_auth::{AuthError, AuthSource};
use echo_config::{ChatGptAuthConfig, LiveConfig, LiveMode};
use std::{io::Write, path::PathBuf, sync::Arc};
use tokio::sync::watch;

/// Cloneable handles share one mode, auth source and bounded kill notification.
/// Deliberately implements neither `Debug` nor `Display`.
#[derive(Clone)]
pub struct Providers {
    state: Arc<State>,
}

struct State {
    mode: LiveMode,
    auth: Option<AuthSource>,
    kill: watch::Sender<bool>,
    cancellation: watch::Sender<()>,
    credential_file: PathBuf,
}

pub enum OperatorCommand {
    Signin { browser: bool },
    Signout,
    ImportCodex { path: PathBuf },
    Status,
    KillSwitch { enabled: bool },
}

/// Fixed categories only; underlying output/path/body errors are discarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SigninError {
    #[error("{0}")]
    Auth(#[from] AuthError),
    #[error("command unavailable in this credential mode")]
    UnsupportedMode,
    #[error("command output failed")]
    Output,
    #[error("invalid credential file")]
    CredentialFile,
}

/// Construct without reading credentials, discovering home files or signing in.
///
/// # Errors
/// Returns a sanitized auth configuration/client initialization error.
pub fn resolve_mode(live: LiveConfig, auth: ChatGptAuthConfig) -> Result<Providers, SigninError> {
    let source = match live.mode {
        LiveMode::Chatgpt => Some(AuthSource::new(auth, live.credential_file.clone())?),
        LiveMode::ApiKey => None,
    };
    let (kill, _) = watch::channel(false);
    Ok(Providers {
        state: Arc::new(State {
            mode: live.mode,
            auth: source,
            kill,
            cancellation: watch::channel(()).0,
            credential_file: live.credential_file,
        }),
    })
}

impl Providers {
    #[must_use]
    pub fn mode(&self) -> LiveMode {
        self.state.mode
    }

    /// Current value plus bounded notifications; active resource ownership is the host's.
    #[must_use]
    pub fn subscribe_kill(&self) -> watch::Receiver<bool> {
        self.state.kill.subscribe()
    }

    /// Run one command, writing only operator-facing text to the supplied writer.
    /// An output error after a successful commit leaves that credential valid.
    ///
    /// # Errors
    /// Returns fixed output/mode categories or the existing sanitized auth error.
    pub async fn execute(&self, command: OperatorCommand, writer: &mut impl Write) -> Result<(), SigninError> {
        match command {
            OperatorCommand::Status => signin_commands::status(self.state.auth.as_ref(), writer).await,
            OperatorCommand::KillSwitch { enabled } => {
                self.state.kill.send_if_modified(|current| {
                    let changed = *current != enabled;
                    if changed && enabled {
                        self.state.cancellation.send_replace(());
                    }
                    *current = enabled;
                    changed
                });
                let label = if enabled { "on" } else { "off" };
                signin_commands::output(writer, &format!("kill-switch: {label}\n"))
            }
            command => {
                let source = self.state.auth.as_ref().ok_or(SigninError::UnsupportedMode)?;
                signin_commands::setup(source, command, writer).await
            }
        }
    }
}

#[cfg(test)]
mod signin_browser_tests;
