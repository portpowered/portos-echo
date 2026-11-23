//! The session controller composes the ordered device, session, pacer and trace World.
use echo_core_api::{
    Broken, Clock, Command, Commands, DeviceControl, DeviceEvent, DeviceState, DeviceView, EntropyUnavailable, Event,
    Now, OsRng, PendingTrace, Rng, SeededRng, TraceDrain, TraceFailure, World,
};
use echo_link::LinkConnector;
use echo_sub_device::Device;
use echo_wyoming::{ClientOptions, Endpoint};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::time::Instant;

mod pacer;
mod session;
mod slots;
mod voice_reducers;
pub mod witness;
pub(crate) use pacer::{PacerPort, scope};
pub(crate) use session::{SessionPort, call_effect};
use slots::Slots;
use voice_reducers::VoiceReducers;
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

type SessionWorld = World<Device, VoiceReducers, echo_sub_trace::Trace, BridgeClock, SessionRng>;

struct State {
    world: SessionWorld,
    runs: Slots<()>,
    first_violation: Option<Broken>,
    trace_held: bool,
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
        Self::with_pacer(witness, echo_sub_pacer::State::default())
    }

    fn with_pacer(witness: Option<Witness>, pacer: echo_sub_pacer::State) -> Arc<Self> {
        let seed = witness.as_ref().and_then(|witness| witness.seed);
        Arc::new(Self {
            state: Mutex::new(State {
                world: World {
                    device: DeviceState::default(),
                    pacer: voice_reducers::State {
                        playback: pacer,
                        session: echo_sub_session::State::default(),
                    },
                    trace: echo_sub_trace::State::default(),
                    clock: BridgeClock {
                        start: Instant::now(),
                        fixed_epoch: seed.is_some(),
                    },
                    rng: seed.map_or(SessionRng::Os(OsRng), |seed| SessionRng::Seeded(SeededRng::new(seed))),
                },
                runs: Slots::new(1),
                first_violation: None,
                trace_held: false,
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
            _ => state.world.device.view(generation),
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
        let mut event = Event::TraceRow(Some(pending));
        let mut out = Commands::default();
        state.world.step(&mut event, &mut out);
        self.check_step(&mut state);
        if matches!(event, Event::TraceRow(None)) {
            let wake = out.take_trace_wake();
            if wake {
                // Checked writes, including shutdown receipts after a failed
                // fixture, must always be able to release the FIFO.
                state.trace_held = false;
            }
            Ok(wake)
        } else {
            Err(crate::Error::Trace)
        }
    }

    pub(crate) fn trace_tick(&self) -> bool {
        let mut out = Commands::default();
        let mut state = self.state();
        state.world.tick_trace(&mut out);
        self.check_step(&mut state);
        out.take_trace_wake()
    }

    pub(crate) fn trace_deadline(&self) -> Option<Instant> {
        let state = self.state();
        let world = &state.world;
        world
            .trace_deadline()
            .and_then(|deadline| world.clock.start.checked_add(deadline))
    }

    pub(crate) fn close_trace(&self) {
        let mut state = self.state();
        state.trace_held = false;
        state.world.trace.close();
    }

    pub(crate) fn hold_trace(&self, held: bool) {
        self.state().trace_held = held;
    }

    pub(crate) fn has_witness(&self) -> bool {
        self.witness.is_some()
    }

    pub(crate) fn trace_failed(&self) {
        self.trace_outcome(Event::TraceFailed);
    }

    fn trace_outcome(&self, mut event: Event) {
        let mut state = self.state();
        state.world.step(&mut event, &mut Commands::default());
        self.check_step(&mut state);
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
    fn next_row(&self) -> Option<(u64, PendingTrace)> {
        let mut state = self.state();
        if state.trace_held {
            return None;
        }
        state.world.trace.next_row()
    }

    fn completed(&self, through: u64, result: Result<(), TraceFailure>) {
        self.trace_outcome(match result {
            Ok(()) => Event::TraceFlushed { through },
            Err(_) => Event::TraceFailed,
        });
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
