use super::{Prepared, codex_host};
use crate::{Error, bridge_runtime::bridge_screen::Screen};
use echo_config::LiveMode;
use echo_screen_contract::UiState;
use echo_signin::{
    OperatorCommand,
    signin_codex::{self, AccountError},
};
use std::{io::Write, time::Duration};
use tokio::time::Instant;

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
    let supervisor = codex_host::supervisor(&command.config.codex).await?;
    let mut screen =
        Screen::bind(matches!(command.operation, OperatorCommand::Signin { .. }).then_some(&command.config.screen))
            .await?;
    #[cfg(test)]
    if let Some(release) = release {
        let _ = release.send(screen.release_observer());
    }
    let deadline = Instant::now() + Duration::from_secs(300);
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
        }
        result.map_err(|error| match error {
            AccountError::Output => Error::Control,
            AccountError::Deadline => Error::Deadline,
            _ => Error::Authorization,
        })
    })
    .await;
    // Screen cleanup still runs when process cleanup or the command fails.
    let screen = screen.close().await;
    screen.and(result)
}

#[cfg(test)]
#[path = "codex_operator_tests.rs"]
mod tests;
