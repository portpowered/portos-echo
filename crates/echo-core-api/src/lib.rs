//! The synchronous World interface shared by the controller and its services.
mod call;
mod clock;
mod device;
mod pacer;
mod rng;
mod session;
mod trace;

pub use call::*;
pub use clock::{Clock, Now, PausedClock, SystemClock};
pub use device::{CapturePrivacy, DeviceControl, DeviceEvent, DeviceState, DeviceView, LinkState};
pub use pacer::*;
pub use rng::{EntropyUnavailable, OsRng, Rng, SeededRng};
pub use session::*;
pub use trace::{PendingTrace, TraceDrain, TraceFailure, TraceRow};

use std::time::Duration;

pub enum Event<'a> {
    Device { generation: u64, change: DeviceEvent },
    TraceRow(Option<PendingTrace>),
    TraceFlushed { through: u64 },
    TraceFailed,
    Pacer(PacerEvent),
    Session(SessionEvent),
    Call(CallEvent<'a>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Device,
    TraceRow,
    TraceFlushed,
    TraceFailed,
    Pacer,
    Session,
    Call,
}

pub enum Command {
    ApplyDeviceView { generation: u64, view: DeviceView },
    Pacer(PacerCommand),
    Session(SessionCommand),
    Call(CallCommand),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    ApplyDeviceView,
    DrainTrace,
    WriteFrame,
    LipBatch,
    MouthChanged,
    PlayoutFinished,
    StartCapture,
    StopCapture,
    EndSession,
    CutReply,
    OpenCall,
    CloseCall,
    SessionToolDecision,
    OpenPeer,
    ClosePeer,
    ResetReceive,
    DeliverRemoteAudio,
    SendUplinkFrame,
    DropBacklog,
}

#[derive(Default)]
pub struct Commands {
    device: Option<Command>,
    drain_trace: bool,
    pacer: PacerCommands,
    session: SessionCommands,
    call: CallCommands,
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

    pub fn pacer(&mut self) -> &mut PacerCommands {
        &mut self.pacer
    }

    pub fn session(&mut self) -> &mut SessionCommands {
        &mut self.session
    }

    pub fn take_session(&mut self) -> SessionCommands {
        std::mem::take(&mut self.session)
    }

    pub fn take_pacer(&mut self) -> PacerCommands {
        std::mem::take(&mut self.pacer)
    }

    pub fn call(&mut self) -> &mut CallCommands {
        &mut self.call
    }

    pub fn take_call(&mut self) -> CallCommands {
        std::mem::take(&mut self.call)
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

pub struct World<D: Subsystem, P: Subsystem, T: Subsystem, C, R> {
    pub device: D::State,
    pub pacer: P::State,
    pub trace: T::State,
    pub clock: C,
    pub rng: R,
}

impl<D: Subsystem, P: Subsystem, T: Subsystem, C: Clock, R: Rng> World<D, P, T, C, R> {
    pub fn step(&mut self, event: &mut Event, out: &mut Commands) {
        let now = self.clock.now();
        D::on_event(&mut self.device, event, now, out);
        P::on_event(&mut self.pacer, event, now, out);
        T::on_event(&mut self.trace, event, now, out);
    }

    pub fn tick_trace(&mut self, out: &mut Commands) {
        T::on_tick(&mut self.trace, self.clock.now(), out);
    }

    /// Tick the mouth on its own wake, independent of PCM offers and trace flushes.
    pub fn tick_pacer(&mut self, out: &mut Commands) {
        P::on_tick(&mut self.pacer, self.clock.now(), out);
    }

    #[must_use]
    pub fn trace_deadline(&self) -> Option<Duration> {
        T::next_deadline(&self.trace)
    }

    /// # Errors
    /// Reports the first broken invariant, if any.
    pub fn check(&self) -> Result<(), Broken> {
        D::check(&self.device)?;
        P::check(&self.pacer)?;
        T::check(&self.trace)
    }
}
