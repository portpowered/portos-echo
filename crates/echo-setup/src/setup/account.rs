//! Private account dispatch. The bridge remains the only account/grant owner.
use super::wake::bounded;
use crate::{Failure, Result, adb::Adb};
use std::sync::atomic::{AtomicU8, Ordering};
use tokio::time::Instant;

/// A cancelled or ambiguous command cannot authorize another account owner.
/// Only the authored stop operation may establish a stopped remote boundary.
#[derive(Default)]
pub(super) struct AccountOwner(AtomicU8);

const IDLE: u8 = 0;
const ACTIVE: u8 = 1;
const UNSETTLED: u8 = 2;

struct Attempt<'a>(Option<&'a AtomicU8>);

impl Attempt<'_> {
    fn complete(mut self) {
        if let Some(state) = self.0.take() {
            state.store(IDLE, Ordering::Release);
        }
    }
}

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        if let Some(state) = self.0 {
            state.store(UNSETTLED, Ordering::Release);
        }
    }
}

impl AccountOwner {
    fn enter(&self, expected: u8) -> Result<Attempt<'_>> {
        self.0
            .compare_exchange(expected, ACTIVE, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|state| {
                Failure(if state == ACTIVE {
                    "setup_account_busy"
                } else {
                    "setup_account_unsettled"
                })
            })?;
        Ok(Attempt(Some(&self.0)))
    }

    pub(super) async fn run(&self, adb: &Adb, command: AccountCommand, deadline: Instant) -> Result<()> {
        bounded(deadline, async {
            let attempt = self.enter(IDLE)?;
            run(adb, command, deadline).await?;
            attempt.complete();
            Ok(())
        })
        .await
    }

    pub(super) async fn settle(&self, adb: &Adb, deadline: Instant) -> Result<()> {
        bounded(deadline, async {
            let attempt = self.enter(UNSETTLED)?;
            let (success, output) = adb
                .script("stop")
                .await
                .map_err(|_| Failure("setup_account_unsettled"))?;
            if !success || output != "echo-install: stopped\n" {
                return Err(Failure("setup_account_unsettled"));
            }
            attempt.complete();
            Ok(())
        })
        .await
    }
}

/// Fixed authored shell operations; caller text never becomes remote shell code.
#[derive(Clone, Copy)]
pub enum AccountCommand {
    /// Keep an existing `ChatGPT` account, signing in only when absent.
    EnsureSignin,
    /// Deliberately reauthenticate after the runtime reports relink required.
    Signin,
    /// Ask the running bridge's existing link owner to link Port OS.
    Link,
}

impl AccountCommand {
    fn operation(self) -> &'static str {
        match self {
            Self::EnsureSignin => "ensure-signin",
            Self::Signin => "signin",
            Self::Link => "link",
        }
    }
}

pub(super) async fn run(adb: &Adb, command: AccountCommand, deadline: Instant) -> Result<()> {
    bounded(deadline, async {
        let operation = command.operation();
        let (success, output) = adb
            .script(operation)
            .await
            .map_err(|_| Failure("setup_account_failed"))?;
        // Neither a screen Ready frame nor an adb exit without the authored
        // completion frame proves that the account command completed.
        if !success || output != format!("echo-install: {operation} completed\n") {
            return Err(Failure("setup_account_failed"));
        }
        Ok(())
    })
    .await
}
