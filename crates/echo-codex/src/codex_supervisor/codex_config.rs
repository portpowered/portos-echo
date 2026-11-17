use super::ProcessError;
use crate::codex_rpc::Limits;
use std::{
    fs::{self, File, OpenOptions},
    path::PathBuf,
    time::Duration,
};

/// Explicit executable and dedicated home. Overrides are TOML values passed as
/// individual argv entries; no shell evaluation or inherited provider environment.
pub struct SupervisorConfig {
    pub binary: PathBuf,
    pub codex_home: PathBuf,
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
            codex_home,
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

    /// The supervisor creates this empty directory under the dedicated home.
    #[must_use]
    pub fn cwd(&self) -> PathBuf {
        self.codex_home.join("work")
    }

    pub(super) fn prepare(&self) -> Result<File, ProcessError> {
        let path = &self.codex_home;
        if !path.is_absolute() || path.file_name().is_none() || path.exists() {
            return Err(ProcessError::PrivateHome);
        }
        let parent = path.parent().ok_or(ProcessError::PrivateHome)?;
        let parent = parent.canonicalize().map_err(|_| ProcessError::PrivateHome)?;
        if parent.ancestors().any(|p| p.join(".git").exists()) {
            return Err(ProcessError::PrivateHome);
        }
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        builder.create(path).map_err(|_| ProcessError::PrivateHome)?;
        fs::create_dir(self.cwd()).map_err(|_| ProcessError::PrivateHome)?;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path.join(".echo-codex-owner"))
            .map_err(|_| ProcessError::PrivateHome)
    }
}
