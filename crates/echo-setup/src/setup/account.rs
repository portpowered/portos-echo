//! Private account dispatch. The bridge remains the only account/grant owner.
use super::wake::bounded;
use crate::{Failure, Result, adb::Adb};
use tokio::time::Instant;

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
