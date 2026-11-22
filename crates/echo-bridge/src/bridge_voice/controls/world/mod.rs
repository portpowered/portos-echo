//! A1 session composition. The wire task steps World before applying its decision.
use echo_core_api::{
    Clock, Command, Commands, DeviceControl, DeviceEvent, DeviceState, DeviceView, Event, Now, OsRng, World,
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

type DeviceWorld = World<Device, BridgeClock, OsRng>;

struct Controller(Mutex<DeviceWorld>);

impl Controller {
    fn world(&self) -> MutexGuard<'_, DeviceWorld> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
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

fn apply(world: &mut DeviceWorld, generation: u64, change: DeviceEvent) -> DeviceView {
    let mut commands = Commands::default();
    world.step(&Event::Device { generation, change }, &mut commands);
    match commands.take() {
        Some(Command::ApplyDeviceView { view, .. }) => view,
        None => world.device.view(generation),
    }
}

pub(crate) fn connector(endpoint: Endpoint) -> impl LinkConnector {
    let world = World {
        device: DeviceState::default(),
        clock: BridgeClock(Instant::now()),
        rng: OsRng,
    };
    let control = Arc::new(Controller(Mutex::new(world)));
    echo_wyoming::adapter::controlled_connector(endpoint, ClientOptions::default(), control)
}
