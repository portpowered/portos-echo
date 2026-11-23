//! Process entry and Wyoming protocol boundary for the pinned C satellite.

mod core;
mod listener;
mod wire;

pub use core::{Action, Core, Protocol};
pub use wire::Event;

use std::{
    convert::Infallible,
    ffi::{CString, NulError, OsString, c_char, c_int},
    path::PathBuf,
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};

pub const NOTICES: &str = include_str!(concat!(env!("OUT_DIR"), "/notices.txt"));
pub const VERSION: &str = env!("HASSMIC_VERSION");
/// Filesystem endpoint shared by the device role and the on-Echo bridge.
pub const WYOMING_SOCKET: &str = "/data/misc/portos-echo-device/wyoming.sock";
/// Sibling endpoint for Wi-Fi requests from the on-Echo bridge.
pub const WIFI_SOCKET: &str = "/data/misc/portos-echo-device/wifi.sock";

/// Handles a bounded Wi-Fi frame on the device worker. The path is the local
/// endpoint, whose parent is owned by the device role.
#[derive(Clone, Copy)]
pub struct WifiHandler {
    pub respond: fn(&std::path::Path, &[u8]) -> Vec<u8>,
    pub monotonic: fn() -> std::time::Duration,
}

#[cfg(target_os = "android")]
pub const WIFI_DEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/classes.dex"));

static ENTERED: AtomicBool = AtomicBool::new(false);

#[expect(unsafe_code, reason = "The satellite exposes a process entry through C")]
unsafe extern "C" {
    fn echo_hassmic_main(argc: c_int, argv: *mut *mut c_char) -> c_int;
}

#[derive(Debug)]
pub enum Error {
    Nul(NulError),
    TooManyArgs,
    AlreadyEntered,
}

/// Takes over the process and exits with the satellite's status.
///
/// # Errors
/// Returns an error before entering C for an interior NUL, an unrepresentable
/// argument count or a second entry in the same process.
pub fn run(
    arguments: impl IntoIterator<Item = OsString>,
    socket: Option<PathBuf>,
    protocol: &'static dyn Protocol,
    wifi: Option<WifiHandler>,
) -> Result<Infallible, Error> {
    let mut storage = std::iter::once(OsString::from("hassmic"))
        .chain(arguments)
        .map(|arg| CString::new(arg.into_encoded_bytes()).map(CString::into_bytes_with_nul))
        .collect::<Result<Vec<_>, _>>()
        .map_err(Error::Nul)?;
    let count = c_int::try_from(storage.len()).map_err(|_| Error::TooManyArgs)?;
    let mut pointers: Vec<_> = storage
        .iter_mut()
        .map(|bytes| bytes.as_mut_ptr().cast::<c_char>())
        .collect();
    pointers.push(ptr::null_mut());
    if ENTERED.swap(true, Ordering::AcqRel) {
        return Err(Error::AlreadyEntered);
    }
    listener::select(socket, wifi);
    core::register(protocol);
    #[expect(unsafe_code, reason = "Enter the C satellite once with owned mutable argv")]
    // SAFETY: argc is the checked count before the NULL sentinel. C may reorder
    // the pointer array and read or modify the NUL-terminated buffers. Both stay
    // live until process exit, including if C returns with workers still active.
    let status = unsafe { echo_hassmic_main(count, pointers.as_mut_ptr()) };
    listener::cleanup();
    std::process::exit(status);
}
