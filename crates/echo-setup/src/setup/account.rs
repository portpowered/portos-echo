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
    pub(super) fn unsettled(&self) -> bool {
        self.0.load(Ordering::Acquire) == UNSETTLED
    }

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

/// Observe public prompts while the authored command owns authentication. A
/// successful command may need no prompt (an existing account/link is retained).
pub(super) async fn guide(
    connection: &super::InstalledConnection,
    command: AccountCommand,
    deadline: Instant,
    now_unix: u64,
    mut present: impl FnMut(&super::Prompt) -> Result<()>,
) -> Result<()> {
    let started = Instant::now();
    let forwards = connection.forward().await?;
    let result = {
        let provider = match command {
            AccountCommand::EnsureSignin | AccountCommand::Signin => super::Provider::Chatgpt,
            AccountCommand::Link => super::Provider::Portos,
        };
        let pending = connection.account(command);
        let prompts = observe(
            connection,
            &forwards,
            provider,
            deadline,
            (started, now_unix),
            &mut present,
        );
        tokio::pin!(pending, prompts);
        tokio::select! {
            biased;
            result = &mut pending => result,
            result = &mut prompts => result,
        }
    };
    // Both futures are dropped before settlement. Local adb cancellation alone
    // cannot authorize another remote owner. Always drain forward cleanup even
    // when settlement fails, and give cleanup refusals diagnostic priority.
    let settlement = connection.recover_account().await;
    let cleanup = forwards.close().await;
    settlement.and(cleanup).and(result)
}

async fn observe(
    connection: &super::InstalledConnection,
    forwards: &super::ForwardSession,
    provider: super::Provider,
    deadline: Instant,
    clock: (Instant, u64),
    present: &mut impl FnMut(&super::Prompt) -> Result<()>,
) -> Result<()> {
    let (started, now_unix) = clock;
    let mut events = bounded(deadline, async {
        loop {
            let connected = match connection.pairing().await {
                Ok(pairing) => forwards.screen(&pairing, deadline).await,
                Err(error) => Err(error),
            };
            match connected {
                Ok(events) => return Ok(events),
                // Signin creates its own screen listener/pairing file after
                // acquiring the stopped runtime's account lease. Poll only
                // startup absence/transport failures, never malformed frames.
                Err(Failure("setup_private_unavailable" | "setup_observation_failed")) => {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                Err(error) => return Err(error),
            }
        }
    })
    .await?;
    let mut expiry = deadline;
    loop {
        let now = now_unix.saturating_add(started.elapsed().as_secs());
        let prompt = tokio::time::timeout_at(expiry, events.next_prompt(provider, now))
            .await
            .map_err(|_| {
                Failure(if expiry < deadline {
                    "setup_code_expired"
                } else {
                    "setup_timeout"
                })
            })??;
        expiry = prompt.expiry(started, now_unix).unwrap_or(deadline).min(deadline);
        if Instant::now() >= expiry {
            return Err(Failure("setup_code_expired"));
        }
        present(&prompt)?;
        events.presented(prompt);
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
    pub(super) fn context(self, error: Failure) -> Failure {
        match (self, error.0) {
            (Self::EnsureSignin | Self::Signin, "setup_timeout") => Failure("setup_chatgpt_timeout"),
            (Self::Link, "setup_timeout") => Failure("setup_portos_timeout"),
            (Self::EnsureSignin | Self::Signin, "setup_code_expired") => Failure("setup_chatgpt_code_expired"),
            (Self::Link, "setup_code_expired") => Failure("setup_portos_code_expired"),
            _ => error,
        }
    }

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
