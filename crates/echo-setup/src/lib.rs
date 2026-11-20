//! Native host orchestration. Device lifecycle policy belongs to echo-install.sh.
mod adb;
pub mod cli;
pub mod doctor;
pub mod install;
pub mod integrity;
pub mod lifecycle;
mod release;
mod version;

/// Fixed public diagnostics never contain dependency output or caller data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failure(pub &'static str);

pub type Result<T> = std::result::Result<T, Failure>;

/// Authored lifecycle and release helper travel as one stdin program.
pub const DEVICE_SCRIPT: &str = concat!(
    include_str!("../../../scripts/echo-install-release.sh"),
    "\n",
    include_str!("../../../scripts/echo-install.sh")
);
