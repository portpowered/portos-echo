use echo_core_api::{DeviceControl, DeviceEvent, DeviceState, DeviceView};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// The standalone client API uses the common transitions without a bridge.
#[derive(Default)]
pub(crate) struct Standalone(Mutex<DeviceState>);

impl Standalone {
    fn state(&self) -> MutexGuard<'_, DeviceState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl DeviceControl for Standalone {
    fn connect(&self) -> (u64, DeviceView) {
        let mut state = self.state();
        let generation = state.next_generation();
        let view = state.apply(generation, DeviceEvent::Connected).unwrap_or_default();
        (generation, view)
    }

    fn observe(&self, generation: u64, change: DeviceEvent) -> DeviceView {
        let mut state = self.state();
        state
            .apply(generation, change)
            .unwrap_or_else(|| state.view(generation))
    }
}

pub(crate) struct Connection {
    control: Arc<dyn DeviceControl>,
    generation: u64,
    view: DeviceView,
}

impl Connection {
    pub(crate) fn new(control: Arc<dyn DeviceControl>) -> Self {
        let (generation, view) = control.connect();
        Self {
            control,
            generation,
            view,
        }
    }

    pub(crate) fn observe(&mut self, change: DeviceEvent) -> DeviceView {
        self.view = self.control.observe(self.generation, change);
        self.view
    }

    pub(crate) fn view(&self) -> DeviceView {
        self.view
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.observe(DeviceEvent::Disconnected);
    }
}
