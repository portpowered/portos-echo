use super::{HomeMode, ProcessError};
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::ErrorKind,
    path::{Component, Path, PathBuf},
};

const BASE_MARKER: &str = ".echo-codex-owner";
const API_MARKER: &str = ".echo-codex-api-key-owner";

pub(super) struct ReservedHome {
    pub(super) path: PathBuf,
    pub(super) base_lease: File,
    api_lease: Option<File>,
}

impl ReservedHome {
    pub(super) fn prepare(base: &Path, mode: HomeMode) -> Result<Self, ProcessError> {
        let base_lease = reserve(base, BASE_MARKER, API_MARKER, true)?.ok_or(ProcessError::PrivateHome)?;
        let mut home = Self {
            path: base.to_path_buf(),
            base_lease,
            api_lease: None,
        };
        if mode != HomeMode::Single {
            let api_path = api_path(base)?;
            let api_mode = mode == HomeMode::ApiKey;
            home.api_lease = reserve(&api_path, API_MARKER, BASE_MARKER, api_mode)?;
            if home.api_lease.is_some() {
                check_auth(&api_path, !api_mode)?;
            }
            if api_mode {
                home.path = api_path;
            }
        }
        Ok(home)
    }

    pub(super) fn cwd(&self) -> PathBuf {
        self.path.join("work")
    }

    pub(super) fn unlock(&self) {
        if let Some(lease) = &self.api_lease {
            let _ = lease.unlock();
        }
        let _ = self.base_lease.unlock();
    }
}

fn api_path(base: &Path) -> Result<PathBuf, ProcessError> {
    let mut name = base.file_name().ok_or(ProcessError::PrivateHome)?.to_os_string();
    name.push("-api-key");
    Ok(base.with_file_name(name))
}

fn check_auth(home: &Path, clean: bool) -> Result<(), ProcessError> {
    let path = home.join("auth.json");
    if let Some(metadata) = entry(&path)? {
        if !unaliased_file(&metadata) {
            return Err(ProcessError::PrivateHome);
        }
        if clean {
            fs::remove_file(path).map_err(|_| ProcessError::PrivateHome)?;
        }
    }
    Ok(())
}

fn reserve(path: &Path, marker: &str, other_marker: &str, create: bool) -> Result<Option<File>, ProcessError> {
    validate_path(path)?;
    let fresh = entry(path)?.is_none();
    if fresh && !create {
        return Ok(None);
    }
    let work = path.join("work");
    if fresh {
        create_home(path)?;
        create_home(&work)?;
    }
    private_directory(path)?;
    validate_path(&path.canonicalize().map_err(|_| ProcessError::PrivateHome)?)?;
    if entry(&path.join(other_marker))?.is_some() {
        return Err(ProcessError::PrivateHome);
    }
    let marker = path.join(marker);
    if !fresh && !entry(&marker)?.is_some_and(|metadata| unaliased_file(&metadata)) {
        return Err(ProcessError::PrivateHome);
    }
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(fresh)
        .open(marker)
        .map_err(|_| ProcessError::PrivateHome)?;
    lease.try_lock().map_err(|_| ProcessError::PrivateHome)?;
    private_directory(&work)?;
    Ok(Some(lease))
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
        if !fs::symlink_metadata(ancestor)
            .map_err(|_| ProcessError::PrivateHome)?
            .is_dir()
        {
            return Err(ProcessError::PrivateHome);
        }
        reject_repository(ancestor)?;
    }
    reject_repository(path)
}

fn reject_repository(path: &Path) -> Result<(), ProcessError> {
    if entry(&path.join(".git"))?.is_some() {
        return Err(ProcessError::PrivateHome);
    }
    Ok(())
}

fn entry(path: &Path) -> Result<Option<Metadata>, ProcessError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(_) => Err(ProcessError::PrivateHome),
    }
}

fn unaliased_file(metadata: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return false;
        }
    }
    metadata.is_file()
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
    let metadata = fs::symlink_metadata(path).map_err(|_| ProcessError::PrivateHome)?;
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

#[cfg(all(test, unix))]
mod auth_mode_home;
