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
        mut present: impl FnMut(&Prompt) -> Result<()>,
        mut ask_wake: impl FnMut() -> Result<()>,
    ) -> Result<()> {
        let started = tokio::time::Instant::now();
        self.authenticate(AccountCommand::EnsureSignin, now_unix, &mut present)
            .await?;
        if self.runtime().await?.relink_required {
            let now = now_unix.saturating_add(started.elapsed().as_secs());
            self.authenticate(AccountCommand::Signin, now, &mut present).await?;
            if self.runtime().await?.relink_required {
                return Err(Failure("setup_relink_required"));
            }
        }
        let forwards = self.forward().await?;
        let required = async {
            let pairing = self.pairing().await?;
            forwards.link_required(&pairing, self.deadline).await
        }
        .await;
        let required = forwards.close().await.and(required)?;
        if required {
            let now = now_unix.saturating_add(started.elapsed().as_secs());
            self.guide_account(AccountCommand::Link, now, &mut present).await?;
        }
        if !self.runtime().await?.runtime_ready() {
            return Err(Failure("setup_relink_required"));
        }
        let wake = WakeConfirmation::selected(self.adb.clone(), self.deadline).await?;
        ask_wake()?;
        wake.wait().await?;
        Ok(())
    }

    async fn authenticate(
        &self,
        command: AccountCommand,
        now_unix: u64,
        present: &mut impl FnMut(&Prompt) -> Result<()>,
    ) -> Result<()> {
        // An earlier dropped account owner must be settled explicitly. Do not
        // bypass it with a new lifecycle operation or a replacement connection.
        self.recover_account().await?;
        wake::bounded(self.deadline, async {
            let (success, output) = self.adb.script("stop").await?;
            if !success || output != "echo-install: stopped\n" {
                return Err(Failure("stop_failed"));
            }
            Ok(())
        })
        .await?;
        self.guide_account(command, now_unix, present).await?;
        self.start().await
    }

    async fn runtime(&self) -> Result<Health> {
        let forwards = self.forward().await?;
        let result = wake::bounded(self.deadline, async {
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
        })
        .await;
        // Even deadline expiry must drain this invocation's allocations before
        // its terminal result; never remove unrelated forwards or raw logs.
        forwards.close().await.and(result)
    }
}

#[cfg(test)]
mod tests;
