use super::ProcessError;
use crate::codex_rpc::Limits;
use std::{
    fs::{self, File, OpenOptions},
    path::{Component, Path, PathBuf},
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
        validate_path(path)?;
        let fresh = match fs::symlink_metadata(path) {
            Ok(_) => false,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(_) => return Err(ProcessError::PrivateHome),
        };
        if fresh {
            create_home(path)?;
        }
        private_directory(path)?;
        validate_path(&path.canonicalize().map_err(|_| ProcessError::PrivateHome)?)?;
        let marker = path.join(".echo-codex-owner");
        if !fresh && !plain_metadata(&marker)?.is_file() {
            return Err(ProcessError::PrivateHome);
        }
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(fresh)
            .open(&marker)
            .map_err(|_| ProcessError::PrivateHome)?;
        lease.try_lock().map_err(|_| ProcessError::PrivateHome)?;
        if fresh {
            fs::create_dir(self.cwd()).map_err(|_| ProcessError::PrivateHome)?;
        }
        if !plain_metadata(&self.cwd())?.is_dir() {
            return Err(ProcessError::PrivateHome);
        }
        Ok(lease)
    }
}

fn validate_path(path: &Path) -> Result<(), ProcessError> {
    if !path.is_absolute()
        || path.file_name().is_none()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(ProcessError::PrivateHome);
    }
    let parent = path.parent().ok_or(ProcessError::PrivateHome)?;
    let canonical = parent.canonicalize().map_err(|_| ProcessError::PrivateHome)?;
    for ancestor in parent.ancestors().chain(canonical.ancestors()) {
        if !plain_metadata(ancestor)?.is_dir() {
            return Err(ProcessError::PrivateHome);
        }
        reject_repository(ancestor)?;
    }
    reject_repository(path)
}

fn reject_repository(path: &Path) -> Result<(), ProcessError> {
    match fs::symlink_metadata(path.join(".git")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(ProcessError::PrivateHome),
    }
}

fn plain_metadata(path: &Path) -> Result<fs::Metadata, ProcessError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ProcessError::PrivateHome)?;
    if metadata.file_type().is_symlink() {
        return Err(ProcessError::PrivateHome);
    }
    Ok(metadata)
}

fn create_home(path: &Path) -> Result<(), ProcessError> {
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder.create(path).map_err(|_| ProcessError::PrivateHome)
}

fn private_directory(path: &Path) -> Result<(), ProcessError> {
    let metadata = plain_metadata(path)?;
    if !metadata.is_dir() {
        return Err(ProcessError::PrivateHome);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o700 {
            return Err(ProcessError::PrivateHome);
        }
    }
    Ok(())
}
