//! The bridge owns screen lifetime and publishes bounded activity updates.
use crate::bridge_mcp::bridge_presentation::Presentation;
use crate::{Error, bridge_input::loopback};
use echo_config::ScreenConfig;
use echo_contracts::secret::Secret;
use echo_screen_contract::UiState;
use echo_screen_server::ScreenServer;
use std::io::Read;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
mod mute;

pub(super) struct Screen {
    mute: Arc<Mutex<mute::Mute>>,
    server: Option<ScreenServer>,
    actions: Option<tokio::task::JoinHandle<()>>,
    pub(crate) presentation: Presentation,
}

impl Screen {
    pub fn address(&self) -> Option<std::net::SocketAddr> {
        self.server.as_ref().map(|server| server.bound_addr)
    }

    pub async fn bind(config: Option<&ScreenConfig>) -> Result<Self, Error> {
        let Some(config) = config else {
            return Ok(Self {
                mute: Arc::default(),
                server: None,
                actions: None,
                presentation: Presentation::new(None),
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
            mute: Arc::default(),
            server: Some(server),
            presentation: Presentation::new(Some(states.clone())),
            actions: None,
        };
        Ok(screen)
    }

    pub fn publish(&self, state: UiState) -> Result<(), Error> {
        if self.muted()? && state != UiState::Muted {
            return Ok(());
        }
        self.presentation
            .publish(self.mute.lock().map_err(|_| Error::Screen)?.project(state))
    }

    pub fn muted(&self) -> Result<bool, Error> {
        Ok(self.mute.lock().map_err(|_| Error::Screen)?.muted)
    }

    pub fn received(&self) -> Result<(), Error> {
        self.mute.lock().map_err(|_| Error::Screen)?.received();
        Ok(())
    }

    pub fn sent(&self) -> Result<(), Error> {
        self.mute.lock().map_err(|_| Error::Screen)?.sent();
        Ok(())
    }

    pub fn observe<R>(
        &self,
        event: &echo_link::LinkEvent<R>,
        trace: &mut crate::bridge_trace::Trace,
    ) -> Result<bool, Error> {
        let changed = self.mute.lock().map_err(|_| Error::Screen)?.event(event, trace)?;
        if changed {
            self.presentation.muted(self.muted()?)?;
        }
        self.muted()
    }

    pub fn ready(&self) -> Result<(), Error> {
        self.publish(UiState::Ready {
            message: "Echo Live ready".into(),
        })
    }

    pub fn supervise_actions(&mut self, health: tokio::sync::watch::Receiver<crate::bridge_health::Links>) {
        let Some(server) = &mut self.server else {
            return;
        };
        let mut actions = std::mem::replace(&mut server.actions, mpsc::channel(1).1);
        let mute = self.mute.clone();
        let presentation = self.presentation.clone();
        self.actions = Some(tokio::spawn(async move {
            while let Some(action) = actions.recv().await {
                if action == echo_screen_contract::UiAction::Relink {
                    // This is an instruction, never an implicit sign-in request.
                    if mute.lock().map_or(true, |mute| mute.muted) {
                        continue;
                    }
                    let result = if health.borrow().relink_required {
                        presentation.publish(failure_state(true))
                    } else {
                        presentation.refresh()
                    };
                    if result.is_err() {
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
