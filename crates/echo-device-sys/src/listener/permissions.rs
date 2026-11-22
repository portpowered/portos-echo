//! Filesystem permissions for the unchanged Android launch identities.

#![expect(unsafe_code, reason = "POSIX identity and pre-thread umask calls")]

use std::{ffi::c_uint, fs::Metadata, io, path::Path};

unsafe extern "C" {
    fn getuid() -> c_uint;
    fn umask(mask: c_uint) -> c_uint;
}

pub(super) fn uid() -> u32 {
    // SAFETY: getuid has no inputs and does not change process state.
    unsafe { getuid() }
}

pub(super) struct SocketMask(c_uint);

impl SocketMask {
    pub(super) fn new() -> Self {
        // SAFETY: The generated entry calls this before any Rust or native workers start.
        Self(unsafe { umask(0o117) })
    }
}

impl Drop for SocketMask {
    fn drop(&mut self) {
        // SAFETY: This restores the launch mask before any workers start.
        unsafe { umask(self.0) };
    }
}

#[cfg(not(target_os = "android"))]
pub(super) fn with_socket_group<T>(_path: &Path, bind: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    bind()
}

pub(super) fn verify_socket(path: &Path, metadata: &Metadata) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let expected = !managed(path) || (metadata.uid() == SYSTEM && metadata.gid() == BRIDGE_GROUP);
    if expected && metadata.mode() & 0o777 == 0o660 {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "unexpected Wyoming socket ownership or mode",
    ))
}

const SYSTEM: u32 = 1000;
const BRIDGE_GROUP: u32 = 3990;

fn managed(path: &Path) -> bool {
    cfg!(target_os = "android") && path == Path::new(crate::WYOMING_SOCKET)
}

#[cfg(target_os = "android")]
unsafe extern "C" {
    fn getegid() -> c_uint;
    fn setegid(gid: c_uint) -> std::ffi::c_int;
}

#[cfg(target_os = "android")]
pub(super) fn with_socket_group<T>(path: &Path, bind: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    if !managed(path) {
        return bind();
    }
    // runas leaves 3990 as the real gid and system as effective and saved.
    // Restrict the temporary effective gid to socket creation, before any
    // native audio calls, which require the unchanged effective group.
    // SAFETY: These calls operate on the single startup thread's credentials.
    let original = unsafe { getegid() };
    set_group(BRIDGE_GROUP)?;
    let result = prepare_directory(path).and_then(|()| bind());
    set_group(original)?;
    result
}

#[cfg(target_os = "android")]
fn set_group(gid: u32) -> io::Result<()> {
    // SAFETY: gid is one of the launcher's real/effective/saved groups.
    if unsafe { setegid(gid) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "android")]
fn prepare_directory(path: &Path) -> io::Result<()> {
    use std::{
        fs::{self, DirBuilder, Permissions},
        os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    };
    let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let created = match DirBuilder::new().mode(0o700).create(parent) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(error),
    };
    let metadata = fs::symlink_metadata(parent)?;
    if !metadata.is_dir() || metadata.uid() != SYSTEM || metadata.gid() != BRIDGE_GROUP {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unexpected Wyoming directory owner or type",
        ));
    }
    if created {
        fs::set_permissions(parent, Permissions::from_mode(0o750))?;
    } else if metadata.mode() & 0o7777 != 0o750 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unexpected Wyoming directory mode",
        ));
    }
    Ok(())
}
