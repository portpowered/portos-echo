use super::Controller;
use crate::bridge_runtime::bridge_pacing::reply_lips::Lips;
use echo_core_api::{Command, Commands, Event, PacerCommand, PacerCommands, PacerEvent, PacerStatus};
use std::{future::Future, sync::Arc, time::Duration};
use tokio::time::Instant;

tokio::task_local! {
    static CURRENT: (Arc<Controller>, Option<Lips>);
}

/// Bind the voice adapter and its optional screen sender to the runtime World.
pub(crate) async fn scope<F: Future>(world: Arc<Controller>, lips: Option<Lips>, work: F) -> F::Output {
    let delay = lips.as_ref().map(Lips::delay);
    dispatch(&world, &mut PacerEvent::ConfigureLips { delay });
    CURRENT.scope((world, lips), work).await
}

fn dispatch(world: &Controller, event: &mut PacerEvent) -> PacerCommands {
    let mut state = world.state();
    let mut out = Commands::default();
    let mut envelope = Event::Pacer(std::mem::replace(event, PacerEvent::Tick));
    state.world.step(&mut envelope, &mut out);
    if let Event::Pacer(recovered) = envelope {
        *event = recovered;
    }
    world.check_step(&mut state);
    out.pacer
}

pub(crate) struct PacerPort(Arc<Controller>);

impl PacerPort {
    pub(crate) fn open(lead_ms: u16, adaptive: bool) -> Self {
        let world = CURRENT
            .try_with(|(world, _)| world.clone())
            .unwrap_or_else(|_| Controller::new());
        let port = Self(world);
        port.send(PacerEvent::RunOpened { lead_ms, adaptive });
        // The native call starts with the cushion measured on the Echo.
        // Component fixtures supply their own deterministic arrival schedules.
        #[cfg(not(test))]
        port.send(PacerEvent::ExpectJitter);
        port
    }

    pub(crate) fn dispatch(&self, event: &mut PacerEvent) -> PacerCommands {
        dispatch(&self.0, event)
    }

    pub(crate) fn send(&self, mut event: PacerEvent) -> PacerCommands {
        let mut out = self.dispatch(&mut event);
        if matches!(out.notification(), Some(Command::Pacer(PacerCommand::LipBatch)))
            && let Some(batch) = out.lips.take()
        {
            let taken = CURRENT
                .try_with(|(_, lips)| lips.as_ref().is_some_and(|lips| lips.send(&batch)))
                .unwrap_or(false);
            self.dispatch(&mut PacerEvent::LipDelivered { taken });
        }
        out
    }

    pub(crate) fn status(&self) -> PacerStatus {
        self.0.state().world.pacer.status()
    }

    pub(crate) fn instant(&self, offset: Option<Duration>) -> Option<Instant> {
        offset.and_then(|offset| self.0.state().world.clock.start.checked_add(offset))
    }

    pub(crate) fn handed(&self) {
        let screen_wanted = CURRENT
            .try_with(|(_, lips)| lips.as_ref().and_then(Lips::wanted))
            .ok()
            .flatten();
        self.send(PacerEvent::FrameHanded { screen_wanted });
    }
}

impl Drop for PacerPort {
    fn drop(&mut self) {
        self.dispatch(&mut PacerEvent::RunClosed);
    }
}
