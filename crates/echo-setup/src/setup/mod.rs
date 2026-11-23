//! Owned local forwards and read-only setup observers. Completion belongs to the command owner,
//! never to an arbitrary screen message or a closed SSE connection.
mod account;
mod confirmation;
mod connection;
mod events;
mod forward;
mod http;
mod prompt;
mod wake;

pub use account::AccountCommand;
pub use confirmation::{ConfirmationEvidence, WakeConfirmation};
pub use connection::InstalledConnection;
pub(crate) use connection::installed_auth_mode;
pub use forward::ForwardSession;
pub use http::{Health, ScreenEvents, health, link_required};
pub use prompt::{Prompt, Provider};
pub use wake::{WakeAttempt, WakeEvidence};

use crate::{Failure, Result};
use std::time::Duration;

impl InstalledConnection {
    /// Guide an installed bridge through exclusive signin, runtime/link and a
    /// fresh wake/run attempt. All operations retain this connection's selected
    /// device and absolute deadline. Authentication and grant mutations remain
    /// with the authored shell and the bridge's existing account/grant owners.
    /// Returned account failures settle remote ownership before returning.
    /// Dropping the future still requires explicit account settlement.
    /// # Errors
    /// Fixed lifecycle, account, observation, presentation or cleanup categories.
    pub async fn guide(
        &self,
        now_unix: u64,
        present: impl FnMut(&Prompt) -> Result<()>,
        ask_wake: impl FnMut() -> Result<()>,
    ) -> Result<()> {
        self.guide_cancellable(now_unix, present, ask_wake, &crate::Cancellation::default())
            .await
    }

    /// Orderly cancellation settles account ownership and awaits forward cleanup.
    /// # Errors
    /// Fixed setup or cleanup categories; cleanup refusal takes priority.
    pub async fn guide_cancellable(
        &self,
        now_unix: u64,
        mut present: impl FnMut(&Prompt) -> Result<()>,
        mut ask_wake: impl FnMut() -> Result<()>,
        cancelled: &crate::Cancellation,
    ) -> Result<()> {
        let started = tokio::time::Instant::now();
        if self.auth_mode() == crate::install::api_key::AuthMode::ApiKey {
            // No ChatGPT sign-in exists in api_key mode. The bridge must still
            // report its configured mode; a refused key is not resumable here.
            self.verify_api_key(cancelled).await?;
            if self.runtime(cancelled).await?.relink_required {
                return Err(Failure("setup_api_key_rejected"));
            }
        } else {
            self.chatgpt(now_unix, started, &mut present, cancelled).await?;
        }
        let forwards = self.forward().await?;
        let required = cancelled
            .until(async {
                let pairing = self.pairing().await?;
                forwards.link_required(&pairing, self.deadline).await
            })
            .await;
        let required = forwards.close().await.and(required)?;
        if required {
            let now = now_unix.saturating_add(started.elapsed().as_secs());
            account::guide(self, AccountCommand::Link, self.deadline, now, &mut present, cancelled)
                .await
                .map_err(|error| AccountCommand::Link.context(error))?;
        }
        if !self.runtime(cancelled).await?.runtime_ready() {
            return Err(Failure("setup_relink_required"));
        }
        let wake = cancelled
            .until(WakeConfirmation::selected(self.adb.clone(), self.deadline))
            .await?;
        ask_wake()?;
        cancelled.until(wake.wait()).await?;
        Ok(())
    }

    async fn authenticate(
        &self,
        command: AccountCommand,
        now_unix: u64,
        present: &mut impl FnMut(&Prompt) -> Result<()>,
        cancelled: &crate::Cancellation,
    ) -> Result<()> {
        // An earlier dropped account owner must be settled explicitly. Do not
        // bypass it with a new lifecycle operation or a replacement connection.
        self.recover_account().await?;
        cancelled.until(async { Ok(()) }).await?;
        wake::bounded(self.deadline, async {
            let (success, output) = self.adb.script("stop").await?;
            if !success || output != "echo-install: stopped\n" {
                return Err(Failure("stop_failed"));
            }
            Ok(())
        })
        .await?;
        account::guide(self, command, self.deadline, now_unix, present, cancelled)
            .await
            .map_err(|error| command.context(error))?;
        cancelled.until(async { Ok(()) }).await?;
        self.start().await
    }

    async fn chatgpt(
        &self,
        now_unix: u64,
        started: tokio::time::Instant,
        present: &mut impl FnMut(&Prompt) -> Result<()>,
        cancelled: &crate::Cancellation,
    ) -> Result<()> {
        self.authenticate(AccountCommand::EnsureSignin, now_unix, present, cancelled)
            .await?;
        if self.runtime(cancelled).await?.relink_required {
            let now = now_unix.saturating_add(started.elapsed().as_secs());
            self.authenticate(AccountCommand::Signin, now, present, cancelled)
                .await?;
            if self.runtime(cancelled).await?.relink_required {
                return Err(Failure("setup_relink_required"));
            }
        }
        Ok(())
    }

    async fn verify_api_key(&self, cancelled: &crate::Cancellation) -> Result<()> {
        self.recover_account().await?;
        cancelled.until(async { Ok(()) }).await?;
        wake::bounded(self.deadline, async {
            let (success, output) = self.adb.script("stop").await?;
            if !success || output != "echo-install: stopped\n" {
                return Err(Failure("stop_failed"));
            }
            Ok(())
        })
        .await?;
        self.account(AccountCommand::ApiKeyStatus)
            .await
            .map_err(|_| Failure("setup_api_key_unconfirmed"))?;
        cancelled.until(async { Ok(()) }).await?;
        self.start().await
    }

    async fn runtime(&self, cancelled: &crate::Cancellation) -> Result<Health> {
        let forwards = self.forward().await?;
        let result = cancelled
            .until(wake::bounded(self.deadline, async {
                loop {
                    let observed = forwards.health(self.deadline).await?;
                    if observed.kill_switch {
                        return Err(Failure("setup_kill_switch"));
                    }
                    if observed.runtime_ready() || observed.relink_required {
                        return Ok(observed);
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }))
            .await;
        // Even deadline expiry must drain this invocation's allocations before
        // its terminal result; never remove unrelated forwards or raw logs.
        forwards.close().await.and(result)
    }
}

#[cfg(test)]
mod tests;
