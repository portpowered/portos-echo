use crate::AuthError;
use std::{
    fs::{File, OpenOptions, TryLockError},
    path::Path,
};

pub(crate) struct Lock(File);

impl std::ops::Deref for Lock {
    type Target = File;

    fn deref(&self) -> &File {
        &self.0
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // A concurrent process spawn can inherit the open file description until
        // exec. Closing only our handle would leave its flock held in that child.
        // End ownership explicitly, including on error and future cancellation.
        let _ = self.0.unlock();
    }
}

// Keep the empty lock file: unlinking it would let another caller lock a new
// inode while a previous caller still owns the old one. OS releases on crash.
pub(crate) fn lock(path: &Path) -> Result<Lock, AuthError> {
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
    Ok(Lock(file))
}

pub(crate) fn pending(file: &File) -> Result<bool, AuthError> {
    Ok(file.metadata().map_err(|_| AuthError::Storage)?.len() != 0)
}
pub(crate) fn mark(file: &File, pending: bool) -> Result<(), AuthError> {
    file.set_len(u64::from(pending))
        .and_then(|()| file.sync_all())
        .map_err(|_| AuthError::Storage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn released_operation_unlocks_even_with_a_duplicate_handle() {
        let directory = crate::auth_private::Directory::new(&std::env::temp_dir()).unwrap();
        let path = directory.0.join("credential");
        let owner = lock(&path).unwrap();
        mark(&owner, true).unwrap();
        // A fork duplicates the open file description before CLOEXEC closes it.
        // Model that interval without unsafe fork/pre_exec or timing a child.
        let inherited = owner.try_clone().unwrap();
        assert_eq!(lock(&path).err(), Some(AuthError::Busy));
        drop(owner);
        let next = lock(&path).unwrap();
        assert!(pending(&next).unwrap());
        drop(inherited);
        // Closing the duplicate must not release the next owner's lock.
        assert_eq!(lock(&path).err(), Some(AuthError::Busy));
        drop(next);
        let next = lock(&path).unwrap();
        assert!(pending(&next).unwrap());
        assert_eq!(std::fs::read(path.with_file_name("credential.lock")).unwrap(), [0]);
    }
}
