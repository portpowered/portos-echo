//! A grant prompt overlays activity, then restores the latest run state.
use crate::Error;
use echo_screen_contract::UiState;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

pub(crate) const RELINK: &str = "Run echo-bridge link with your config to relink Port OS MCP.";

#[derive(Clone)]
pub(crate) struct Presentation {
    states: Option<mpsc::Sender<UiState>>,
    current: Arc<Mutex<(UiState, Option<UiState>, bool)>>,
}

impl Presentation {
    pub fn new(states: Option<mpsc::Sender<UiState>>) -> Self {
        Self {
            states,
            current: Arc::new(Mutex::new((
                UiState::Ready {
                    message: "Ready".into(),
                },
                None,
                false,
            ))),
        }
    }

    pub fn publish(&self, state: UiState) -> Result<(), Error> {
        let mut current = self.current.lock().map_err(|_| Error::Screen)?;
        current.0 = state;
        self.send(if current.2 {
            UiState::Muted
        } else {
            current.1.as_ref().unwrap_or(&current.0).clone()
        })
    }

    pub fn overlay(&self, state: Option<UiState>) -> Result<(), Error> {
        let mut current = self.current.lock().map_err(|_| Error::Screen)?;
        current.1 = state;
        self.send(if current.2 {
            UiState::Muted
        } else {
            current.1.as_ref().unwrap_or(&current.0).clone()
        })
    }

    pub fn muted(&self, muted: bool) -> Result<(), Error> {
        let mut current = self.current.lock().map_err(|_| Error::Screen)?;
        current.2 = muted;
        if !muted {
            // Muting ends the run; restore idle before exposing activity again.
            current.0 = UiState::Ready {
                message: "Echo Live ready".into(),
            };
        }
        self.send(if muted {
            UiState::Muted
        } else {
            current.1.as_ref().unwrap_or(&current.0).clone()
        })
    }

    fn send(&self, state: UiState) -> Result<(), Error> {
        if let Some(states) = &self.states {
            states.try_send(state).map_err(|_| Error::Screen)?;
        }
        Ok(())
    }

    pub fn rejected(&self, message: &str) -> Result<(), Error> {
        self.overlay(Some(UiState::Error {
            message: message.into(),
            relink: true,
        }))
    }

    pub fn refresh(&self) -> Result<(), Error> {
        let current = self.current.lock().map_err(|_| Error::Screen)?;
        self.send(if current.2 {
            UiState::Muted
        } else {
            current.1.as_ref().unwrap_or(&current.0).clone()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmute_emits_only_restored_idle_or_the_active_overlay() {
        let ready = UiState::Ready {
            message: "Echo Live ready".into(),
        };
        let error = UiState::Error {
            message: RELINK.into(),
            relink: true,
        };
        for base in [None, Some(UiState::Listening), Some(UiState::Speaking)] {
            for overlay in [None, Some(error.clone())] {
                let (send, mut receive) = mpsc::channel(8);
                let screen = Presentation::new(Some(send));
                if let Some(base) = base.clone() {
                    screen.publish(base.clone()).unwrap();
                    assert_eq!(receive.try_recv().unwrap(), base);
                }
                screen.muted(true).unwrap();
                assert_eq!(receive.try_recv().unwrap(), UiState::Muted);
                screen.overlay(overlay.clone()).unwrap();
                assert_eq!(receive.try_recv().unwrap(), UiState::Muted);
                screen.muted(false).unwrap();
                assert_eq!(receive.try_recv().unwrap(), overlay.unwrap_or(ready.clone()));
                assert!(receive.try_recv().is_err());
                screen.overlay(None).unwrap();
                assert_eq!(receive.try_recv().unwrap(), ready);
                assert!(receive.try_recv().is_err());
            }
        }
    }

    #[test]
    fn mcp_prompts_and_actions_preserve_the_mute_screen_until_unmuted() {
        let (send, mut receive) = mpsc::channel(16);
        let screen = Presentation::new(Some(send));
        let prompt = UiState::LinkCode {
            user_code: "TEST-CODE".into(),
            verification_uri: "http://127.0.0.1:1/approve".into(),
            expires_at_unix: 1800,
        };
        screen.muted(true).unwrap();
        screen.overlay(Some(prompt.clone())).unwrap();
        screen.publish(UiState::Speaking).unwrap();
        screen.refresh().unwrap();
        for _ in 0..4 {
            assert_eq!(receive.try_recv().unwrap(), UiState::Muted);
        }
        screen.muted(false).unwrap();
        assert_eq!(receive.try_recv().unwrap(), prompt);
        screen.muted(true).unwrap();
        screen.rejected(RELINK).unwrap();
        screen.refresh().unwrap();
        for _ in 0..3 {
            assert_eq!(receive.try_recv().unwrap(), UiState::Muted);
        }
        screen.muted(false).unwrap();
        assert_eq!(
            receive.try_recv().unwrap(),
            UiState::Error {
                message: RELINK.into(),
                relink: true
            }
        );
        assert!(receive.try_recv().is_err());
    }

    #[test]
    fn mcp_link_prompt_restores_run_state_and_rejection_survives_activity() {
        let (send, mut receive) = mpsc::channel(8);
        let screen = Presentation::new(Some(send));
        let prompt = UiState::LinkCode {
            user_code: "TEST-CODE".into(),
            verification_uri: "http://127.0.0.1:1/approve".into(),
            expires_at_unix: 1800,
        };
        screen.overlay(Some(prompt.clone())).unwrap();
        screen.publish(UiState::Speaking).unwrap();
        screen.overlay(None).unwrap();
        screen.rejected(RELINK).unwrap();
        screen.publish(UiState::Listening).unwrap();
        assert_eq!(receive.try_recv().unwrap(), prompt);
        assert_eq!(receive.try_recv().unwrap(), prompt);
        assert_eq!(receive.try_recv().unwrap(), UiState::Speaking);
        let error = UiState::Error {
            message: RELINK.into(),
            relink: true,
        };
        assert_eq!(receive.try_recv().unwrap(), error);
        assert_eq!(receive.try_recv().unwrap(), error);
    }
}
