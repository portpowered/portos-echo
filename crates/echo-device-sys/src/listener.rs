//! Owns the local Wyoming endpoint for the lifetime of the native satellite.

#![expect(unsafe_code, reason = "Native listener and POSIX termination hooks")]

mod permissions;

use std::{
    ffi::{CString, c_char, c_int, c_void},
    fs::{self, File, Metadata},
    io::{self, ErrorKind},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            fs::{FileTypeExt, MetadataExt},
            net::{UnixDatagram, UnixListener},
        },
    },
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicI32, Ordering},
    },
};

static PATH: OnceLock<PathBuf> = OnceLock::new();
static ENDPOINT: Mutex<Option<Endpoint>> = Mutex::new(None);
static STOP: OnceLock<UnixDatagram> = OnceLock::new();
static NOTIFY: AtomicI32 = AtomicI32::new(-1);

unsafe extern "C" {
    fn echo_hassmic_probe_unix(path: *const c_char) -> c_int;
    fn echo_hassmic_open_lease(path: *const c_char) -> c_int;
    fn echo_hassmic_on_stop(notify: extern "C" fn(c_int)) -> c_int;
    fn write(fd: c_int, buffer: *const c_void, length: usize) -> isize;
}

pub(super) fn select(path: Option<PathBuf>) {
    if let Some(path) = path {
        let _ = PATH.set(path);
    }
}

pub(super) fn selected() -> bool {
    PATH.get().is_some()
}

// The native entry calls this after the log fork and before starting its threads.
// -2 selects the host TCP listener, -1 is startup failure, and an fd selects Unix.
#[unsafe(no_mangle)]
extern "C" fn echo_hassmic_prepare_listener() -> c_int {
    let Some(path) = PATH.get() else {
        return if cfg!(target_os = "android") { -1 } else { -2 };
    };
    match prepare(path) {
        Ok(fd) => fd,
        Err(error) => {
            log(format_args!("hassmic: Wyoming socket {}: {error}", path.display()));
            -1
        }
    }
}

fn prepare(path: &Path) -> io::Result<c_int> {
    let endpoint = permissions::with_socket_group(path, || Endpoint::bind(path))?;
    let fd = endpoint.listener.as_raw_fd();
    {
        let mut owner = ENDPOINT
            .lock()
            .map_err(|_| io::Error::other("listener owner poisoned"))?;
        *owner = Some(endpoint);
    }
    watch_stop()?;
    Ok(fd)
}

#[unsafe(no_mangle)]
extern "C" fn echo_hassmic_unix_ready() {
    if let Some(path) = PATH.get() {
        log(format_args!("hassmic: wyoming on Unix socket {}", path.display()));
    }
}

fn log(message: std::fmt::Arguments<'_>) {
    use io::Write;
    let _ = writeln!(io::stderr().lock(), "{message}");
}

pub(super) fn cleanup() {
    if let Ok(mut owner) = ENDPOINT.lock() {
        // Release the lease only after unlinking the endpoint it protects.
        drop(owner.take());
    }
}

fn watch_stop() -> io::Result<()> {
    let (receive, send) = UnixDatagram::pair()?;
    send.set_nonblocking(true)?;
    let fd = send.as_raw_fd();
    STOP.set(send)
        .map_err(|_| io::Error::other("termination already installed"))?;
    NOTIFY.store(fd, Ordering::Release);
    std::thread::Builder::new().name("wyoming-stop".into()).spawn(move || {
        if receive.recv(&mut [0]).is_ok() {
            cleanup();
            std::process::exit(0);
        }
    })?;
    // SAFETY: The callback uses only a lock-free descriptor load and POSIX write.
    if unsafe { echo_hassmic_on_stop(notify) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

extern "C" fn notify(_signal: c_int) {
    let fd = NOTIFY.load(Ordering::Acquire);
    // SAFETY: STOP keeps this nonblocking descriptor live. write is async-signal-safe.
    unsafe { write(fd, b"x".as_ptr().cast(), 1) };
}

struct Endpoint {
    listener: UnixListener,
    path: PathBuf,
    inode: (u64, u64),
    _lease: File,
}

impl Endpoint {
    fn bind(path: &Path) -> io::Result<Self> {
        // Before starting native threads, create the socket as 0660, including
        // when the launcher's ambient umask grants broader permissions.
        let _mask = permissions::SocketMask::new();
        let lease = lock(path)?;
        reclaim(path)?;
        let listener = UnixListener::bind(path)?;
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.file_type().is_socket() {
            return Err(conflict());
        }
        let endpoint = Self {
            listener,
            path: path.into(),
            inode: identity(&metadata),
            _lease: lease,
        };
        permissions::verify_socket(path, &metadata)?;
        Ok(endpoint)
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        if let Err(error) = unlink_owned(&self.path, self.inode) {
            log(format_args!("hassmic: Wyoming socket cleanup: {error}"));
        }
    }
}

fn lock(path: &Path) -> io::Result<File> {
    // Keep the sidecar inode across restarts so two processes cannot lock two
    // different files for the same path. The kernel releases the lock on exit.
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(".lock");
    let sidecar = c_path(Path::new(&sidecar))?;
    // SAFETY: sidecar is a live NUL-terminated pathname. The wrapper returns
    // a new CLOEXEC descriptor and refuses to follow a final symlink.
    let fd = unsafe { echo_hassmic_open_lease(sidecar.as_ptr()) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: This is the first Rust owner of the returned descriptor.
    let file = unsafe { File::from_raw_fd(fd) };
    let meta = file.metadata()?;
    if !meta.is_file() || meta.nlink() != 1 || meta.mode() & 0o777 != 0o600 || meta.uid() != permissions::uid() {
        return Err(conflict());
    }
    file.try_lock().map_err(|error| match error {
        fs::TryLockError::WouldBlock => io::Error::new(ErrorKind::AddrInUse, "Wyoming socket already owned"),
        fs::TryLockError::Error(error) => error,
    })?;
    Ok(file)
}

fn reclaim(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() || metadata.uid() != permissions::uid() {
        return Err(conflict());
    }
    let endpoint = c_path(path)?;
    // SAFETY: The C function borrows the NUL-terminated path and closes its
    // nonblocking probe descriptor before returning the connect errno.
    let status = unsafe { echo_hassmic_probe_unix(endpoint.as_ptr()) };
    if status == 0 {
        return Err(io::Error::new(ErrorKind::AddrInUse, "Wyoming listener is live"));
    }
    let error = io::Error::from_raw_os_error(status);
    if error.kind() != ErrorKind::ConnectionRefused {
        return Err(error);
    }
    unlink_owned(path, identity(&metadata))
}

fn unlink_owned(path: &Path, inode: (u64, u64)) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() && identity(&metadata) == inode => fs::remove_file(path),
        Ok(_) => Err(conflict()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn identity(metadata: &Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_encoded_bytes()).map_err(|error| io::Error::new(ErrorKind::InvalidInput, error))
}

fn conflict() -> io::Error {
    io::Error::new(ErrorKind::AlreadyExists, "conflicting Wyoming endpoint entry")
}
