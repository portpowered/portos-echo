//! Unmounted Muse core. A report is a result, never user consent or approval.
#[cfg(test)]
extern crate self as echo_muse_bridge;

mod backend;
mod clock;
mod gadget;
mod report;
mod store;

pub use backend::{MuseBackend, compose_task_message};
pub use clock::{Clock, EpochClock};
pub use gadget::{CliGadget, Gadget, GadgetError, Reply, SideChat};
pub use report::{ReportError, ReportIntake, handler_output, report_command_spec};
pub use store::{InMemoryStore, JsonFileStore, Record, Snapshot, StoreError, TaskState, TaskStore};

#[cfg(test)]
mod tests;
