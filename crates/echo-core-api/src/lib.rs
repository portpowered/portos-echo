//! The synchronous World interface shared by the controller and its services.
mod clock;
mod device;
mod rng;
mod trace;

pub use clock::{Clock, Now, PausedClock, SystemClock};
pub use device::{CapturePrivacy, DeviceControl, DeviceEvent, DeviceState, DeviceView, LinkState};
pub use rng::{EntropyUnavailable, OsRng, Rng, SeededRng};
pub use trace::{PendingTrace, TraceDrain, TraceFailure, TraceRow};

use std::time::Duration;

pub enum Event {
    Device { generation: u64, change: DeviceEvent },
    Trace(Option<PendingTrace>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Device,
    Trace,
}

pub enum Command {
    ApplyDeviceView { generation: u64, view: DeviceView },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    ApplyDeviceView,
    DrainTrace,
}

#[derive(Default)]
pub struct Commands {
    device: Option<Command>,
    drain_trace: bool,
}

impl Commands {
    pub fn publish_device_view(&mut self, generation: u64, view: DeviceView) {
        self.device = Some(Command::ApplyDeviceView { generation, view });
    }

    pub fn take(&mut self) -> Option<Command> {
        self.device.take()
    }

    pub fn drain_trace(&mut self) {
        self.drain_trace = true;
    }

    pub fn take_trace_wake(&mut self) -> bool {
        std::mem::take(&mut self.drain_trace)
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

    fn on_event(state: &mut Self::State, event: &mut Event, now: Now, out: &mut Commands);
    fn on_tick(_state: &mut Self::State, _now: Now, _out: &mut Commands) {}
    fn next_deadline(state: &Self::State) -> Option<Duration>;
    /// # Errors
    /// Reports the first broken invariant, if any.
    fn check(state: &Self::State) -> Result<(), Broken>;
}

pub struct World<D: Subsystem, T: Subsystem, C, R> {
    pub device: D::State,
    pub trace: T::State,
    pub clock: C,
    pub rng: R,
}

impl<D: Subsystem, T: Subsystem, C: Clock, R: Rng> World<D, T, C, R> {
    pub fn step(&mut self, event: &mut Event, out: &mut Commands) {
        let now = self.clock.now();
        D::on_event(&mut self.device, event, now, out);
        T::on_event(&mut self.trace, event, now, out);
    }

    pub fn tick(&mut self, out: &mut Commands) {
        let now = self.clock.now();
        D::on_tick(&mut self.device, now, out);
        T::on_tick(&mut self.trace, now, out);
    }

    #[must_use]
    pub fn next_deadline(&self) -> Option<Duration> {
        D::next_deadline(&self.device)
            .into_iter()
            .chain(T::next_deadline(&self.trace))
            .min()
    }

    /// # Errors
    /// Reports the first broken invariant, if any.
    pub fn check(&self) -> Result<(), Broken> {
        D::check(&self.device)?;
        T::check(&self.trace)
    }
}
