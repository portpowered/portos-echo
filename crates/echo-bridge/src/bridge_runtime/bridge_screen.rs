//! The bridge owns screen lifetime and publishes bounded activity updates.
use crate::{Error, bridge_input::loopback};
use echo_config::ScreenConfig;
use echo_contracts::secret::Secret;
use echo_screen_contract::UiState;
use echo_screen_server::ScreenServer;
use std::io::Read;
use tokio::sync::mpsc;

pub(super) struct Screen {
    server: Option<ScreenServer>,
    states: Option<mpsc::Sender<UiState>>,
    actions: Option<tokio::task::JoinHandle<()>>,
}

impl Screen {
    pub fn address(&self) -> Option<std::net::SocketAddr> {
        self.server.as_ref().map(|server| server.bound_addr)
    }

    pub async fn bind(config: Option<&ScreenConfig>) -> Result<Self, Error> {
        let Some(config) = config else {
            return Ok(Self {
                server: None,
                states: None,
                actions: None,
            });
        };
        let bind = loopback(&config.listen.to_string(), true)?;
        let file = std::fs::File::open(&config.pairing_token_file).map_err(|_| Error::Screen)?;
        let mut bytes = Vec::new();
        file.take(16_385).read_to_end(&mut bytes).map_err(|_| Error::Screen)?;
        if bytes.len() > 16_384 {
            return Err(Error::Screen);
        }
        let text = String::from_utf8(bytes).map_err(|_| Error::Screen)?;
        let token = text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix('\n'))
            .unwrap_or(&text);
        if token.trim().is_empty() || token.chars().any(char::is_control) {
            return Err(Error::Screen);
        }
        let (states, input) = mpsc::channel(32);
        let server = echo_screen_server::serve(bind, Secret::new(token.into()), input)
            .await
            .map_err(|_| Error::Screen)?;
        let screen = Self {
            server: Some(server),
            states: Some(states),
            actions: None,
        };
        screen.ready()?;
        Ok(screen)
    }

    pub fn publish(&self, state: UiState) -> Result<(), Error> {
        if let Some(states) = &self.states {
            states.try_send(state).map_err(|_| Error::Screen)?;
        }
        Ok(())
    }

    pub fn ready(&self) -> Result<(), Error> {
        self.publish(UiState::Ready {
            message: "Ready".into(),
        })
    }

    pub fn supervise_actions(&mut self, health: tokio::sync::watch::Receiver<crate::bridge_health::Links>) {
        let (Some(server), Some(states)) = (&mut self.server, &self.states) else {
            return;
        };
        let mut actions = std::mem::replace(&mut server.actions, mpsc::channel(1).1);
        let states = states.clone();
        self.actions = Some(tokio::spawn(async move {
            while let Some(action) = actions.recv().await {
                if action == echo_screen_contract::UiAction::Relink && health.borrow().relink_required {
                    // This is an instruction, never an implicit sign-in request.
                    if states.send(failure_state(true)).await.is_err() {
                        break;
                    }
                }
            }
        }));
    }

    pub async fn close(&mut self) -> Result<(), Error> {
        if let Some(task) = self.actions.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(server) = &mut self.server {
            server.shutdown().await.map_err(|_| Error::Screen)?;
        }
        Ok(())
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        if let Some(task) = &self.actions {
            task.abort();
        }
    }
}

pub(super) fn failure_state(relink: bool) -> UiState {
    UiState::Error {
        message: if relink {
            "Run echo-bridge signin with your config, then restart the bridge."
        } else {
            "Conversation unavailable. Try another wake."
        }
        .into(),
        relink,
    }
}
