//! The synchronous World interface shared by the controller and its services.
mod clock;
mod device;
mod rng;

pub use clock::{Clock, Now, PausedClock, SystemClock};
pub use device::{CapturePrivacy, DeviceControl, DeviceEvent, DeviceState, DeviceView, LinkState};
pub use rng::{EntropyUnavailable, OsRng, Rng, SeededRng};

use std::time::Duration;

pub enum Event {
    Device { generation: u64, change: DeviceEvent },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Device,
}

pub enum Command {
    ApplyDeviceView { generation: u64, view: DeviceView },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    ApplyDeviceView,
}

/// A1's device reducer publishes at most one view per step.
#[derive(Default)]
pub struct Commands(Option<Command>);

impl Commands {
    pub fn publish_device_view(&mut self, generation: u64, view: DeviceView) {
        self.0 = Some(Command::ApplyDeviceView { generation, view });
    }

    pub fn take(&mut self) -> Option<Command> {
        self.0.take()
    }
}

/// One safe invariant failure, with no transport or customer payloads.
#[derive(Debug, PartialEq, Eq)]
pub struct Broken {
    pub field: &'static str,
    pub expected: &'static str,
    pub actual: &'static str,
}

pub trait Subsystem {
    type State;
    const INPUTS: &'static [EventKind];
    const OUTPUTS: &'static [CommandKind];

    fn on_event(state: &mut Self::State, event: &Event, now: Now, out: &mut Commands);
    fn on_tick(_state: &mut Self::State, _now: Now, _out: &mut Commands) {}
    fn next_deadline(state: &Self::State) -> Option<Duration>;
    /// # Errors
    /// Reports the first broken invariant, if any.
    fn check(state: &Self::State) -> Result<(), Broken>;
}

pub struct World<D: Subsystem, C, R> {
    pub device: D::State,
    pub clock: C,
    pub rng: R,
}

impl<D: Subsystem, C: Clock, R: Rng> World<D, C, R> {
    pub fn step(&mut self, event: &Event, out: &mut Commands) {
        D::on_event(&mut self.device, event, self.clock.now(), out);
    }

    pub fn tick(&mut self, out: &mut Commands) {
        D::on_tick(&mut self.device, self.clock.now(), out);
    }

    #[must_use]
    pub fn next_deadline(&self) -> Option<Duration> {
        D::next_deadline(&self.device)
    }

    /// # Errors
    /// Reports the first broken invariant, if any.
    pub fn check(&self) -> Result<(), Broken> {
        D::check(&self.device)
    }
}
