use super::ProcessError;
use super::codex_home::ReservedHome;
use crate::codex_rpc::Limits;
use std::{path::PathBuf, time::Duration};

/// How the executable is asked to serve stdio. The pinned multi-tool takes the
/// `app-server` subcommand; a standalone app-server build takes only its flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Launch {
    #[default]
    Subcommand,
    Standalone,
}

/// Credential storage policy for the dedicated app-server home.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HomeMode {
    #[default]
    Single,
    Chatgpt,
    ApiKey,
}

/// Explicit executable and dedicated home. Overrides are TOML values passed as
/// individual argv entries; no shell evaluation or inherited provider environment.
pub struct SupervisorConfig {
    pub binary: PathBuf,
    pub launch: Launch,
    pub codex_home: PathBuf,
    pub home_mode: HomeMode,
    pub config_overrides: Vec<(String, String)>,
    pub request_timeout: Duration,
    pub max_line_bytes: usize,
    pub max_pending_requests: usize,
    pub max_notifications: usize,
    pub restart_limit: u32,
    pub restart_window: Duration,
}

impl SupervisorConfig {
    #[must_use]
    pub fn new(binary: PathBuf, codex_home: PathBuf) -> Self {
        let limits = Limits::default();
        Self {
            binary,
            launch: Launch::default(),
            codex_home,
            home_mode: HomeMode::default(),
            config_overrides: Vec::new(),
            request_timeout: limits.request_timeout,
            max_line_bytes: limits.line_bytes,
            max_pending_requests: limits.pending_requests,
            max_notifications: limits.notifications,
            restart_limit: 2,
            restart_window: Duration::from_secs(60),
        }
    }

    pub(super) fn limits(&self) -> Limits {
        Limits {
            line_bytes: self.max_line_bytes,
            pending_requests: self.max_pending_requests,
            notifications: self.max_notifications,
            request_timeout: self.request_timeout,
        }
    }

    pub(super) fn prepare(&self) -> Result<ReservedHome, ProcessError> {
        ReservedHome::prepare(&self.codex_home, self.home_mode)
    }
}
