//! Opt-in Unix service. All mutation belongs to one leased backend.
mod api;
mod config;
mod control;
mod daemon;
mod ipc;
mod state;
mod transport;

pub use daemon::run;
pub use ipc::run_handler;

#[cfg(test)]
mod tests;
