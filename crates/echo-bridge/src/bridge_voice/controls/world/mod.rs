//! A1 session composition. Services share the ordered device and trace World.
use echo_core_api::{
    Clock, Command, Commands, DeviceControl, DeviceEvent, DeviceState, DeviceView, Event, Now, OsRng, PendingTrace,
    TraceDrain, World,
};
use echo_link::LinkConnector;
use echo_sub_device::Device;
use echo_wyoming::{ClientOptions, Endpoint};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::time::Instant;

struct BridgeClock(Instant);

impl Clock for BridgeClock {
    fn now(&self) -> Now {
        Now {
            monotonic: self.0.elapsed(),
            wall: crate::bridge_time::now(),
        }
    }
}

type SessionWorld = World<Device, echo_sub_trace::Trace, BridgeClock, OsRng>;

pub(crate) struct Controller(Mutex<SessionWorld>);

impl Controller {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(World {
            device: DeviceState::default(),
            trace: echo_sub_trace::State::default(),
            clock: BridgeClock(Instant::now()),
            rng: OsRng,
        })))
    }

    fn world(&self) -> MutexGuard<'_, SessionWorld> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn enqueue(&self, mut pending: PendingTrace, started: Instant) -> Result<bool, crate::Error> {
        let mut world = self.0.lock().map_err(|_| crate::Error::Trace)?;
        pending.row.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut event = Event::Trace(Some(pending));
        let mut out = Commands::default();
        world.step(&mut event, &mut out);
        if matches!(event, Event::Trace(None)) {
            Ok(out.take_trace_wake())
        } else {
            Err(crate::Error::Trace)
        }
    }

    pub(crate) fn trace_tick(&self) -> bool {
        let mut out = Commands::default();
        self.world().tick(&mut out);
        out.take_trace_wake()
    }

    pub(crate) fn trace_deadline(&self) -> Option<Instant> {
        let world = self.world();
        world
            .next_deadline()
            .and_then(|deadline| world.clock.0.checked_add(deadline))
    }

    pub(crate) fn close_trace(&self) {
        self.world().trace.close();
    }

    pub(crate) fn current_run(&self) -> Result<Option<String>, crate::Error> {
        Ok(self.0.lock().map_err(|_| crate::Error::Trace)?.trace.current_run())
    }

    pub(crate) fn set_run(&self, run: Option<&str>) -> Result<(), crate::Error> {
        self.0.lock().map_err(|_| crate::Error::Trace)?.trace.set_run(run);
        Ok(())
    }
}

impl TraceDrain for Controller {
    fn next_row(&self) -> Option<PendingTrace> {
        self.world().trace.next_row()
    }
}

impl DeviceControl for Controller {
    fn connect(&self) -> (u64, DeviceView) {
        let mut world = self.world();
        let generation = world.device.next_generation();
        (generation, apply(&mut world, generation, DeviceEvent::Connected))
    }

    fn observe(&self, generation: u64, change: DeviceEvent) -> DeviceView {
        apply(&mut self.world(), generation, change)
    }
}

fn apply(world: &mut SessionWorld, generation: u64, change: DeviceEvent) -> DeviceView {
    let mut commands = Commands::default();
    world.step(&mut Event::Device { generation, change }, &mut commands);
    match commands.take() {
        Some(Command::ApplyDeviceView { view, .. }) => view,
        None => world.device.view(generation),
    }
}

pub(crate) fn connector(endpoint: Endpoint) -> (impl LinkConnector, Arc<Controller>) {
    let control = Controller::new();
    let connector = echo_wyoming::adapter::controlled_connector(endpoint, ClientOptions::default(), control.clone());
    (connector, control)
}
