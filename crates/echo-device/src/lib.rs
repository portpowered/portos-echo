//! Dedicated Wyoming satellite process and its offline notices.

mod protocol;
mod transport;
pub mod wifi;

use std::{
    convert::Infallible,
    ffi::{OsStr, OsString},
    io::{self, Write},
    path::Path,
};

/// Recognizes the basenames used by the device supervisor and the simulator.
#[must_use]
pub fn satellite_name(program: &OsStr) -> bool {
    let basename = Path::new(program).file_name();
    ["hassmic-host", "hassmic"]
        .iter()
        .any(|name| basename == Some(OsStr::new(name)))
}

/// Enters the satellite before the bridge creates any runtime resources.
///
/// # Errors
/// Returns an error if the process entry or argument list is invalid.
pub fn run(args: impl IntoIterator<Item = OsString>) -> Result<Infallible, &'static str> {
    enter(args, None)
}

/// Enters the device role with the sibling Wi-Fi protocol handler enabled.
///
/// # Errors
/// Returns an error if the process entry or argument list is invalid.
pub fn run_with_wifi(
    args: impl IntoIterator<Item = OsString>,
    handler: echo_device_sys::WifiHandler,
) -> Result<Infallible, &'static str> {
    enter(args, Some(handler))
}

fn enter(
    args: impl IntoIterator<Item = OsString>,
    wifi: Option<echo_device_sys::WifiHandler>,
) -> Result<Infallible, &'static str> {
    let (args, socket) = transport::select(args, cfg!(target_os = "android"))?;
    echo_device_sys::run(args, socket, &protocol::WYOMING, wifi).map_err(|error| match error {
        echo_device_sys::Error::Nul(_) => "satellite argument contains a NUL byte",
        echo_device_sys::Error::TooManyArgs => "too many satellite arguments",
        echo_device_sys::Error::AlreadyEntered => "satellite already entered",
    })
}

#[cfg(target_os = "android")]
pub use echo_device_sys::WIFI_DEX;
pub use echo_device_sys::{WIFI_SOCKET, WYOMING_SOCKET, WifiHandler};

/// Writes the licence notices for the vendored satellite.
///
/// # Errors
/// Propagates a failure to write to the destination.
pub fn notices(output: &mut impl Write) -> io::Result<()> {
    output.write_all(echo_device_sys::NOTICES.as_bytes())
}
