//! Single process entry for the pinned C satellite.

use std::{
    convert::Infallible,
    ffi::{CString, NulError, OsString, c_char, c_int},
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};

pub const NOTICES: &str = include_str!(concat!(env!("OUT_DIR"), "/notices.txt"));

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
pub fn run(args: impl IntoIterator<Item = OsString>) -> Result<Infallible, Error> {
    let mut storage = std::iter::once(OsString::from("hassmic"))
        .chain(args)
        .map(|arg| CString::new(arg.into_encoded_bytes()).map(CString::into_bytes_with_nul))
        .collect::<Result<Vec<_>, _>>()
        .map_err(Error::Nul)?;
    let argc = c_int::try_from(storage.len()).map_err(|_| Error::TooManyArgs)?;
    let mut argv: Vec<_> = storage
        .iter_mut()
        .map(|bytes| bytes.as_mut_ptr().cast::<c_char>())
        .collect();
    argv.push(ptr::null_mut());
    if ENTERED.swap(true, Ordering::AcqRel) {
        return Err(Error::AlreadyEntered);
    }
    #[expect(unsafe_code, reason = "Enter the C satellite once with owned mutable argv")]
    // SAFETY: argc is the checked count before the NULL sentinel. C may reorder
    // the pointer array and read or modify the NUL-terminated buffers. Both stay
    // live until process exit, including if C returns with workers still active.
    let status = unsafe { echo_hassmic_main(argc, argv.as_mut_ptr()) };
    std::process::exit(status);
}
