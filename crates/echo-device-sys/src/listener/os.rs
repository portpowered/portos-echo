//! Unix listener operations at the Linux and Android libc boundary.

#![expect(unsafe_code, reason = "Unix socket and signal FFI")]

use std::{
    ffi::{c_int, c_uint, c_void},
    fs::{File, OpenOptions},
    io::{self, ErrorKind},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::fs::OpenOptionsExt,
    },
    path::Path,
    ptr,
};

// Linux UAPI values for the x86-64 host and ARM Echo. ARM uses a different
// O_NOFOLLOW bit from the generic fcntl ABI.
const AF_UNIX: u16 = 1;
const SOCK_STREAM: c_int = 1;
const NONBLOCK: c_int = 0x800;
const SOCK_CLOEXEC: c_int = 0x8_0000;
const O_NOFOLLOW: c_int = if cfg!(any(target_arch = "arm", target_arch = "aarch64")) {
    0x8000
} else {
    0x2_0000
};
const SIGINT: c_int = 2;
const SIGTERM: c_int = 15;
const SIG_ERR: usize = usize::MAX;

#[repr(C)]
struct UnixAddress {
    family: u16,
    path: [u8; 108],
}

unsafe extern "C" {
    fn accept4(fd: c_int, address: *mut c_void, length: *mut c_uint, flags: c_int) -> c_int;
    fn socket(domain: c_int, kind: c_int, protocol: c_int) -> c_int;
    fn connect(fd: c_int, address: *const UnixAddress, length: c_uint) -> c_int;
    fn signal(signum: c_int, handler: extern "C" fn(c_int)) -> usize;
    fn write(fd: c_int, buffer: *const c_void, length: usize) -> isize;
}

#[unsafe(no_mangle)]
extern "C" fn echo_hassmic_accept_unix(fd: c_int) -> c_int {
    loop {
        // SAFETY: The native loop borrows the live listener. Null address
        // outputs discard the peer address. The native loop owns the result.
        let client = unsafe { accept4(fd, ptr::null_mut(), ptr::null_mut(), SOCK_CLOEXEC) };
        if client >= 0 || io::Error::last_os_error().kind() != ErrorKind::Interrupted {
            return client;
        }
    }
}

pub(super) fn probe(path: &Path) -> io::Result<()> {
    let name = path.as_os_str().as_encoded_bytes();
    let mut address = UnixAddress {
        family: AF_UNIX,
        path: [0; 108],
    };
    if name.is_empty() || name.len() >= address.path.len() || name.contains(&0) {
        return Err(ErrorKind::InvalidInput.into());
    }
    address.path[..name.len()].copy_from_slice(name);
    let length = c_uint::try_from(size_of::<UnixAddress>()).map_err(io::Error::other)?;
    // SAFETY: These flags request a new, nonblocking Unix stream descriptor.
    let fd = unsafe { socket(AF_UNIX.into(), SOCK_STREAM | NONBLOCK | SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: socket returned a new descriptor, so this is its sole owner.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: The fully initialized sockaddr_un lives through connect.
    if unsafe { connect(fd.as_raw_fd(), &raw const address, length) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub(super) fn open_lease(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(O_NOFOLLOW | NONBLOCK)
        .open(path)
}

pub(super) fn on_stop(notify: extern "C" fn(c_int)) -> io::Result<()> {
    // SAFETY: The caller provides a signal-safe callback with a static lifetime.
    if unsafe { signal(SIGTERM, notify) == SIG_ERR || signal(SIGINT, notify) == SIG_ERR } {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(super) fn notify(fd: c_int) {
    // SAFETY: The one-byte buffer is valid. POSIX write is async-signal-safe.
    unsafe { write(fd, b"x".as_ptr().cast(), 1) };
}
