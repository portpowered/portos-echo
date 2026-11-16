//! Durable cross-process control; inspection failures refuse work.
use super::bridge_screen::Screen;
use crate::{
    Error,
    bridge_health::Health,
    bridge_input::{Provider, Settings},
    bridge_trace::Trace,
};
use echo_signin::OperatorCommand;
use serde_json::json;
use std::{io::ErrorKind, path::Path, time::Duration};

pub(crate) fn inspect(path: &Path) -> Result<bool, Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() && metadata.len() == 0 => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Ok(_) | Err(_) => Err(Error::Control),
    }
}

pub(crate) fn mutate(path: &Path, enabled: bool) -> Result<(), Error> {
    let present = inspect(path)?;
    if enabled && !present {
        std::fs::create_dir_all(path.parent().ok_or(Error::Control)?).map_err(|_| Error::Control)?;
        match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(file) => file.sync_all().map_err(|_| Error::Control)?,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                inspect(path)?;
            }
            Err(_) => return Err(Error::Control),
        }
    } else if !enabled && present {
        match std::fs::remove_file(path) {
            Ok(()) => (),
            Err(error) if error.kind() == ErrorKind::NotFound => (),
            Err(_) => return Err(Error::Control),
        }
    }
    Ok(())
}

pub(super) struct Kill<'a> {
    pub settings: &'a Settings,
    pub health: &'a Health,
    pub screen: &'a Screen,
}

impl Kill<'_> {
    pub fn enabled(&self) -> bool {
        self.settings
            .kill_path
            .as_deref()
            .is_some_and(|path| inspect(path).unwrap_or(true))
    }

    pub async fn wait(&self) {
        while !self.enabled() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub async fn sync(&self, trace: &mut Trace) -> Result<bool, Error> {
        let enabled = self.enabled();
        if enabled != self.health.state.borrow().kill_switch {
            if let Provider::Resolved(providers) = &self.settings.credential {
                providers
                    .execute(OperatorCommand::KillSwitch { enabled }, &mut std::io::sink())
                    .await
                    .map_err(|_| Error::Control)?;
            }
            self.health.state.send_modify(|state| {
                state.kill_switch = enabled;
                if enabled {
                    state.live = "error";
                } else if !state.relink_required {
                    state.live = "idle";
                }
            });
            trace.record(if enabled { "kill.enabled" } else { "kill.cleared" }, None, json!({}))?;
            if enabled {
                self.screen.publish(echo_screen_contract::UiState::Error {
                    message: "Kill switch enabled. Run echo-bridge kill --clear with your config.".into(),
                    relink: false,
                })?;
            } else if !self.health.state.borrow().relink_required {
                self.screen.ready()?;
            }
        }
        Ok(enabled)
    }
}
