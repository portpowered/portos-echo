//! Device state and its invariants, evaluated synchronously by the World.
use echo_core_api::{Broken, CommandKind, Commands, DeviceState, Event, EventKind, LinkState, Now, Subsystem};
use std::time::Duration;

pub struct Device;

impl Subsystem for Device {
    type State = DeviceState;
    const INPUTS: &'static [EventKind] = &[EventKind::Device];
    const OUTPUTS: &'static [CommandKind] = &[CommandKind::ApplyDeviceView];

    fn on_event(state: &mut Self::State, event: &Event, _now: Now, out: &mut Commands) {
        let Event::Device { generation, change } = *event;
        if let Some(view) = state.apply(generation, change) {
            out.publish_device_view(generation, view);
        }
    }

    fn next_deadline(_: &Self::State) -> Option<Duration> {
        None
    }

    fn check(state: &Self::State) -> Result<(), Broken> {
        let view = state.current();
        if view.capture_open
            && (!view.run_open || view.privacy.mute_latched || view.privacy.mic_muted || view.link == LinkState::Paused)
        {
            return Err(Broken {
                field: "device.capture_open",
                expected: "false",
                actual: "true",
            });
        }
        if view.speaker_volume.is_some_and(|volume| volume > 100) {
            return Err(Broken {
                field: "device.speaker_volume",
                expected: "0..=100",
                actual: "out of range",
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
