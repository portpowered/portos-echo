//! Native host orchestration. Device lifecycle policy belongs to echo-install.sh.
mod adb;
pub mod cli;
pub mod install;
pub mod integrity;
mod release;
mod version;

/// Fixed public diagnostics never contain dependency output or caller data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failure(pub &'static str);

pub type Result<T> = std::result::Result<T, Failure>;
