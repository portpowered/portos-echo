//! A1 session composition. Services share the ordered device and trace World.
use echo_core_api::{
    Broken, Clock, Command, Commands, DeviceControl, DeviceEvent, DeviceState, DeviceView, EntropyUnavailable, Event,
    Now, OsRng, PendingTrace, Rng, SeededRng, TraceDrain, World,
};
use echo_link::LinkConnector;
use echo_sub_device::Device;
use echo_wyoming::{ClientOptions, Endpoint};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::time::Instant;

pub mod witness;
use witness::Witness;

struct BridgeClock {
    start: Instant,
    fixed_epoch: bool,
}

impl Clock for BridgeClock {
    fn now(&self) -> Now {
        Now {
            monotonic: self.start.elapsed(),
            wall: if self.fixed_epoch {
                std::time::UNIX_EPOCH + self.start.elapsed()
            } else {
                crate::bridge_time::now()
            },
        }
    }
}

enum SessionRng {
    Os(OsRng),
    Seeded(SeededRng),
}

impl Rng for SessionRng {
    fn fill(&mut self, dest: &mut [u8]) -> Result<(), EntropyUnavailable> {
        match self {
            Self::Os(rng) => rng.fill(dest),
            Self::Seeded(rng) => rng.fill(dest),
        }
    }
}

type SessionWorld = World<Device, echo_sub_trace::Trace, BridgeClock, SessionRng>;

struct State {
    world: SessionWorld,
    first_violation: Option<Broken>,
    writer_failed: bool,
}

pub(crate) struct Controller {
    state: Mutex<State>,
    witness: Option<Witness>,
}

impl Controller {
    pub(crate) fn new() -> Arc<Self> {
        Self::with_witness(None)
    }

    fn with_witness(witness: Option<Witness>) -> Arc<Self> {
        let seed = witness.as_ref().and_then(|witness| witness.seed);
        Arc::new(Self {
            state: Mutex::new(State {
                world: World {
                    device: DeviceState::default(),
                    trace: echo_sub_trace::State::default(),
                    clock: BridgeClock {
                        start: Instant::now(),
                        fixed_epoch: seed.is_some(),
                    },
                    rng: seed.map_or(SessionRng::Os(OsRng), |seed| SessionRng::Seeded(SeededRng::new(seed))),
                },
                first_violation: None,
                writer_failed: false,
            }),
            witness,
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn check_step(&self, state: &mut State) {
        if self.witness.is_some() && state.first_violation.is_none() {
            state.first_violation = state.world.check().err();
        }
    }

    fn apply(&self, state: &mut State, generation: u64, change: DeviceEvent) -> DeviceView {
        let mut commands = Commands::default();
        state
            .world
            .step(&mut Event::Device { generation, change }, &mut commands);
        self.check_step(state);
        match commands.take() {
            Some(Command::ApplyDeviceView { view, .. }) => view,
            None => state.world.device.view(generation),
        }
    }

    pub(crate) fn publish(&self) -> Result<(), crate::Error> {
        self.witness.as_ref().map_or(Ok(()), |witness| {
            let report = witness::Report::from_state(&self.state());
            witness.publish(&report)
        })
    }

    pub(crate) fn enqueue(&self, mut pending: PendingTrace, started: Instant) -> Result<bool, crate::Error> {
        let mut state = self.state.lock().map_err(|_| crate::Error::Trace)?;
        pending.row.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut event = Event::Trace(Some(pending));
        let mut out = Commands::default();
        state.world.step(&mut event, &mut out);
        self.check_step(&mut state);
        if matches!(event, Event::Trace(None)) {
            Ok(out.take_trace_wake())
        } else {
            Err(crate::Error::Trace)
        }
    }

    pub(crate) fn trace_tick(&self) -> bool {
        let mut out = Commands::default();
        let mut state = self.state();
        state.world.tick(&mut out);
        self.check_step(&mut state);
        out.take_trace_wake()
    }

    pub(crate) fn trace_deadline(&self) -> Option<Instant> {
        let state = self.state();
        let world = &state.world;
        world
            .next_deadline()
            .and_then(|deadline| world.clock.start.checked_add(deadline))
    }

    pub(crate) fn close_trace(&self) {
        self.state().world.trace.close();
    }

    pub(crate) fn current_run(&self) -> Result<Option<String>, crate::Error> {
        Ok(self
            .state
            .lock()
            .map_err(|_| crate::Error::Trace)?
            .world
            .trace
            .current_run())
    }

    pub(crate) fn set_run(&self, run: Option<&str>) -> Result<(), crate::Error> {
        self.state
            .lock()
            .map_err(|_| crate::Error::Trace)?
            .world
            .trace
            .set_run(run);
        Ok(())
    }
}

impl TraceDrain for Controller {
    fn next_row(&self) -> Option<PendingTrace> {
        self.state().world.trace.next_row()
    }

    fn write_failed(&self) {
        self.state().writer_failed = true;
    }
}

impl DeviceControl for Controller {
    fn connect(&self) -> (u64, DeviceView) {
        let mut state = self.state();
        let generation = state.world.device.next_generation();
        (generation, self.apply(&mut state, generation, DeviceEvent::Connected))
    }

    fn observe(&self, generation: u64, change: DeviceEvent) -> DeviceView {
        self.apply(&mut self.state(), generation, change)
    }
}

pub(crate) fn connector(
    endpoint: Endpoint,
    local: bool,
) -> Result<(impl LinkConnector, Arc<Controller>), crate::Error> {
    let witness = Witness::open(local && endpoint.tcp_addr().ip().is_loopback())?;
    let control = Controller::with_witness(witness);
    let connector = echo_wyoming::adapter::controlled_connector(endpoint, ClientOptions::default(), control.clone());
    Ok((connector, control))
}
