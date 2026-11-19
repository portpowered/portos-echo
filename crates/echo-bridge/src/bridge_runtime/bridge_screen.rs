//! The bridge owns screen lifetime and publishes bounded activity updates.
use crate::bridge_mcp::bridge_presentation::Presentation;
use crate::{Error, bridge_input::loopback, bridge_trace::Trace};
use echo_config::ScreenConfig;
use echo_contracts::secret::Secret;
use echo_screen_contract::UiState;
use echo_screen_server::ScreenServer;
use std::io::Read;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
pub(crate) mod approval;
pub(crate) mod emotion;
mod mute;

pub(crate) struct Screen {
    mute: Arc<Mutex<mute::Mute>>,
    settings_warning: Option<String>,
    server: Option<Arc<ScreenServer>>,
    actions: Option<tokio::task::JoinHandle<()>>,
    pack_results: Option<tokio::task::JoinHandle<()>>,
    pub(crate) presentation: Presentation,
    pub(crate) approvals: approval::Approvals,
}

impl Screen {
    #[cfg(test)]
    pub(crate) fn release_observer(&mut self) -> mpsc::Receiver<echo_screen_contract::UiAction> {
        // Signin never consumes actions. The listener router owns every sender;
        // it is destroyed only after the listener and connection tasks end.
        let server = Arc::get_mut(self.server.as_mut().unwrap()).unwrap();
        std::mem::replace(&mut server.actions, mpsc::channel(1).1)
    }

    pub fn address(&self) -> Option<std::net::SocketAddr> {
        self.server.as_ref().map(|server| server.bound_addr)
    }

    pub async fn bind(config: Option<&ScreenConfig>) -> Result<Self, Error> {
        Self::bind_with_settings(config, None).await
    }

    pub(crate) async fn bind_with_settings(
        config: Option<&ScreenConfig>,
        settings: Option<mpsc::Sender<echo_screen_server::SettingsRequest>>,
    ) -> Result<Self, Error> {
        let Some(config) = config else {
            return Ok(Self {
                mute: Arc::default(),
                settings_warning: None,
                server: None,
                actions: None,
                pack_results: None,
                presentation: Presentation::new(None),
                approvals: approval::Approvals::default(),
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
        let server = echo_screen_server::serve_with_settings(bind, Secret::new(token.into()), input, settings)
            .await
            .map_err(|_| Error::Screen)?;
        let screen = Self {
            mute: Arc::default(),
            settings_warning: None,
            server: Some(Arc::new(server)),
            presentation: Presentation::new(Some(states.clone())),
            actions: None,
            pack_results: None,
            approvals: approval::Approvals::default(),
        };
        Ok(screen)
    }

    pub(crate) fn download_packs(&mut self, config: Option<&echo_config::BridgeConfig>, trace: &Trace) {
        let Some(config) = config.filter(|c| !c.screen.packs.is_empty()) else {
            return;
        };
        let Some(home) = &config.home else {
            return;
        };
        let Some(server) = self.server.as_mut().and_then(Arc::get_mut) else {
            return;
        };
        let pins = config
            .screen
            .packs
            .iter()
            .map(|p| (p.url.clone(), p.sha256.clone()))
            .collect();
        let mut results = server.install_configured_packs(home.dir.clone(), config.codex.ca_file.clone(), pins);
        let mut trace = trace.clone();
        self.pack_results = Some(tokio::spawn(async move {
            while let Some(result) = results.recv().await {
                let fields = serde_json::json!({"installed": result.installed, "reason": result.reason});
                if trace.record("screen.pack_download", None, fields).is_err() {
                    break;
                }
            }
        }));
    }

    pub fn publish(&self, state: UiState) -> Result<(), Error> {
        if self.muted()? && state != UiState::Muted {
            return Ok(());
        }
        self.presentation
            .publish(self.mute.lock().map_err(|_| Error::Screen)?.project(state))
    }

    pub async fn publish_confirmed(&self, state: UiState) -> Result<(), Error> {
        self.publish(state)?;
        self.flush().await
    }

    pub(crate) async fn flush(&self) -> Result<(), Error> {
        if let Some(server) = &self.server {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            server.flush(deadline).await.map_err(|_| Error::Screen)?;
        }
        Ok(())
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

    pub(crate) fn settings_warning(
        &mut self,
        warning: Option<&str>,
        trace: &mut crate::bridge_trace::Trace,
    ) -> Result<(), Error> {
        self.settings_warning = warning.map(String::from);
        if warning.is_some() {
            let event = if warning == Some(crate::bridge_settings::RESTART_FAILED) {
                "settings.restart_recovered"
            } else {
                "settings.ignored"
            };
            let fields = if event == "settings.ignored" {
                serde_json::json!({"reason":"invalid_config"})
            } else {
                serde_json::json!({})
            };
            trace.record(event, None, fields)?;
        }
        Ok(())
    }

    pub fn ready(&self) -> Result<(), Error> {
        self.publish(self.settings_warning.as_ref().map_or_else(
            || UiState::Ready {
                message: "Echo Live ready".into(),
            },
            |message| UiState::Error {
                message: message.clone(),
                relink: false,
            },
        ))
    }

    pub fn supervise_actions(&mut self, health: tokio::sync::watch::Receiver<crate::bridge_health::Links>) {
        let Some(server) = &mut self.server else {
            return;
        };
        let Some(owner) = Arc::get_mut(server) else {
            return;
        };
        let mut actions = std::mem::replace(&mut owner.actions, mpsc::channel(1).1);
        if let Some(clock) = crate::bridge_mcp::bridge_clock::receipt::Context::current()
            && clock.screen(server).is_err()
        {
            return;
        }
        let mut drain = match crate::bridge_mcp::bridge_clock::receipt::Context::current() {
            Some(clock) => {
                if let Ok(drain) = clock.publisher() {
                    Some(drain)
                } else {
                    clock.failed();
                    return;
                }
            }
            None => None,
        };
        let mute = self.mute.clone();
        let presentation = self.presentation.clone();
        let approvals = self.approvals.clone();
        self.actions = Some(tokio::spawn(async move {
            loop {
                let action = tokio::select! {
                    biased;
                    result = presentation.advance_emotion() => {
                        if result.is_err() { break; }
                        continue;
                    },
                    action = actions.recv() => { let Some(action) = action else { break; }; action },
                    () = async {
                        match &mut drain {
                            Some(drain) => drain.acknowledge().await,
                            None => std::future::pending().await,
                        }
                    } => continue,
                };
                if !mute.lock().map_or(true, |mute| mute.muted) && approvals.answer(&action).is_err() {
                    break;
                }
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
        self.close_with_mode(false).await.map(|_| ())
    }

    pub(crate) async fn close_gracefully(&mut self) -> Result<usize, Error> {
        self.close_with_mode(true).await
    }

    async fn close_with_mode(&mut self, graceful: bool) -> Result<usize, Error> {
        if let Some(task) = self.pack_results.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.actions.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(server) = self.server.take() {
            let mut server = Arc::try_unwrap(server).map_err(|_| Error::Screen)?;
            return if graceful {
                server.shutdown_gracefully().await.map_err(|_| Error::Screen)
            } else {
                server.shutdown().await.map_err(|_| Error::Screen).map(|()| 0)
            };
        }
        Ok(0)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        if let Some(task) = &self.pack_results {
            task.abort();
        }
        if let Some(task) = &self.actions {
            task.abort();
        }
    }
}

pub(crate) fn failure_state(relink: bool) -> UiState {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// T3: configured pack failure is isolated from the real screen listener.
    #[tokio::test]
    async fn t3_screen_pack_missing_ca_reports_safely_and_keeps_screen_serving() {
        let home = tempfile::tempdir().unwrap();
        let pairing = home.path().join("pairing.txt");
        std::fs::write(&pairing, "synthetic-screen-pair").unwrap();
        let config = echo_config::BridgeConfig {
            home: Some(echo_config::HomeConfig {
                dir: home.path().into(),
                agent_tools: echo_config::AgentTools::default(),
            }),
            screen: ScreenConfig {
                listen: ([127, 0, 0, 1], 0).into(),
                pairing_token_file: pairing,
                packs: vec![echo_config::ScreenPackDownload {
                    url: "https://127.0.0.1/pack".into(),
                    sha256: "0".repeat(64),
                }],
            },
            ..echo_config::BridgeConfig::default()
        };
        let path = home.path().join("trace.jsonl");
        let trace = Trace::open(&path, "api_key").unwrap();
        let mut screen = Screen::bind(Some(&config.screen)).await.unwrap();
        screen.download_packs(Some(&config), &trace);
        screen.pack_results.take().unwrap().await.unwrap();
        let row: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(row["event"], "screen.pack_download");
        assert_eq!(row["installed"], serde_json::Value::Null);
        assert_eq!(row["reason"], "configured CA bundle unavailable");
        screen.ready().unwrap();
        screen.flush().await.unwrap();
        let base = "http://127.0.0.1";
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("{base}:{}/screen.css", screen.address().unwrap().port()))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        screen.close().await.unwrap();
        assert!(!home.path().join("screens").exists());
    }

    #[tokio::test]
    async fn bridge_relink_confirmed_publication_keeps_projection_and_queue_failure() {
        let (send, mut receive) = mpsc::channel(1);
        let screen = Screen {
            mute: Arc::default(),
            settings_warning: None,
            server: None,
            actions: None,
            pack_results: None,
            presentation: Presentation::new(Some(send)),
            approvals: approval::Approvals::default(),
        };
        let overlay = UiState::Error {
            message: "MCP link".into(),
            relink: true,
        };
        screen.presentation.overlay(Some(overlay.clone())).unwrap();
        assert_eq!(receive.try_recv().unwrap(), overlay);
        screen.publish_confirmed(failure_state(true)).await.unwrap();
        assert_eq!(receive.try_recv().unwrap(), overlay);
        screen.presentation.muted(true).unwrap();
        assert_eq!(receive.try_recv().unwrap(), UiState::Muted);
        screen.publish_confirmed(failure_state(true)).await.unwrap();
        assert_eq!(receive.try_recv().unwrap(), UiState::Muted);
        screen.presentation.muted(false).unwrap();
        assert_eq!(receive.try_recv().unwrap(), overlay);
        screen.presentation.overlay(None).unwrap();
        assert_eq!(
            receive.try_recv().unwrap(),
            UiState::Ready {
                message: "Echo Live ready".into()
            }
        );
        screen.publish_confirmed(failure_state(false)).await.unwrap();
        assert_eq!(screen.publish_confirmed(failure_state(true)).await, Err(Error::Screen));
        assert_eq!(receive.try_recv().unwrap(), failure_state(false));
        drop(receive);
        assert_eq!(screen.publish_confirmed(failure_state(true)).await, Err(Error::Screen));
    }
}
