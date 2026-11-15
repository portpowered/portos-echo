use crate::AuthError;
use std::{
    fs::{File, OpenOptions, TryLockError},
    path::Path,
};

// Keep the empty lock file: unlinking it would let another caller lock a new
// inode while a previous caller still owns the old one. OS releases on crash.
pub(crate) fn lock(path: &Path) -> Result<File, AuthError> {
    if path.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(AuthError::InvalidConfig);
    }
    let name = path.file_name().ok_or(AuthError::InvalidConfig)?;
    let mut lock_name = name.to_os_string();
    lock_name.push(".lock");
    let lock_path = path.with_file_name(lock_name);
    if lock_path
        .symlink_metadata()
        .is_ok_and(|m| !m.is_file() || m.file_type().is_symlink())
    {
        return Err(AuthError::InvalidConfig);
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(lock_path).map_err(|_| AuthError::Storage)?;
    file.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => AuthError::Busy,
        TryLockError::Error(_) => AuthError::Storage,
    })?;
    Ok(file)
}

pub(crate) fn pending(file: &File) -> Result<bool, AuthError> {
    Ok(file.metadata().map_err(|_| AuthError::Storage)?.len() != 0)
}
pub(crate) fn mark(file: &File, pending: bool) -> Result<(), AuthError> {
    file.set_len(u64::from(pending))
        .and_then(|()| file.sync_all())
        .map_err(|_| AuthError::Storage)
}
