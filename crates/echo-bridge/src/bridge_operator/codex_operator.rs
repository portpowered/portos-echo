use super::{Prepared, codex_host};
use crate::{Error, bridge_runtime::bridge_screen::Screen};
use echo_config::LiveMode;
use echo_screen_contract::UiState;
use echo_signin::{
    OperatorCommand,
    signin_codex::{self, AccountError, Prompt},
};
use std::{io::Write, time::Duration};
use tokio::time::Instant;

const ACCOUNT_TIMEOUT: Duration = Duration::from_secs(300);

/// Run an account operation after the voice generation has released its home.
pub(crate) async fn screen_account(
    host: &crate::bridge_codex::HostOptions<'_>,
    operation: OperatorCommand,
    cancelled: impl std::future::Future<Output = ()>,
    present: impl FnOnce(&Prompt, Duration) -> Result<(), AccountError>,
) -> Result<(), Error> {
    let deadline = Instant::now() + ACCOUNT_TIMEOUT;
    tokio::pin!(cancelled);
    let supervisor = tokio::select! {
        biased;
        () = &mut cancelled => return Err(Error::Control),
        result = tokio::time::timeout_at(deadline, codex_host::routed_supervisor(host.config, host.mode, host.routes)) => {
            result.map_err(|_| Error::Deadline)??
        }
    };
    codex_host::run(supervisor, cancelled, deadline, async |supervisor| {
        signin_codex::execute(
            supervisor.io().map_err(codex_host::process_error)?,
            host.mode,
            operation,
            &mut std::io::sink(),
            |prompt| present(prompt, deadline.saturating_duration_since(Instant::now())),
            deadline,
        )
        .await
        .map_err(account_error)
    })
    .await
}

fn account_error(error: AccountError) -> Error {
    match error {
        AccountError::Output => Error::Control,
        AccountError::Deadline => Error::Deadline,
        _ => Error::Authorization,
    }
}

pub(super) async fn execute(
    command: Prepared,
    writer: &mut impl Write,
    cancelled: impl std::future::Future<Output = ()>,
    #[cfg(test)] release: Option<
        tokio::sync::oneshot::Sender<tokio::sync::mpsc::Receiver<echo_screen_contract::UiAction>>,
    >,
) -> Result<(), Error> {
    if matches!(command.operation, OperatorCommand::Signin { .. }) && command.config.live.mode != LiveMode::Chatgpt {
        return Err(Error::Authorization);
    }
    let supervisor = codex_host::supervisor(&command.config.codex, command.config.live.mode).await?;
    let mut screen =
        Screen::bind(matches!(command.operation, OperatorCommand::Signin { .. }).then_some(&command.config.screen))
            .await?;
    #[cfg(test)]
    if let Some(release) = release {
        let _ = release.send(screen.release_observer());
    }
    let deadline = Instant::now() + ACCOUNT_TIMEOUT;
    let mut terminal_published = false;
    let result = codex_host::run(supervisor, cancelled, deadline, async |supervisor| {
        let result = signin_codex::execute(
            supervisor.io().map_err(codex_host::process_error)?,
            command.config.live.mode,
            command.operation,
            writer,
            |prompt| {
                screen
                    .publish(UiState::Ready {
                        message: prompt.message(),
                    })
                    .map_err(|_| AccountError::Output)
            },
            deadline,
        )
        .await;
        if result.is_err() {
            screen
                .publish_confirmed(UiState::Error {
                    message: "Run echo-bridge signin with your config, then restart the bridge.".into(),
                    relink: true,
                })
                .await?;
            terminal_published = true;
        }
        result.map_err(account_error)
    })
    .await;
    // Confirmation queues the Error. The HTTP connection still needs to drain it.
    let screen = if terminal_published {
        screen.close_gracefully().await.map(|_| ())
    } else {
        screen.close().await
    };
    screen.and(result)
}

#[cfg(test)]
#[path = "codex_operator_tests.rs"]
mod tests;
