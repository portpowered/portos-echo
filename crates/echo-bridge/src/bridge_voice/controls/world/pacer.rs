use super::Controller;
use crate::bridge_runtime::bridge_pacing::reply_lips::Lips;
use echo_core_api::{Command, Commands, Event, PacerCommand, PacerCommands, PacerEvent, Subsystem};
use echo_sub_pacer::{LipStats, Pacer, State};
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
    let mut envelope = Event::Pacer(std::mem::replace(event, PacerEvent::Tick));
    let out = step(world, &mut world.state(), &mut envelope);
    if let Event::Pacer(recovered) = envelope {
        *event = recovered;
    }
    out
}

fn step(world: &Controller, state: &mut super::State, event: &mut Event) -> PacerCommands {
    let mut out = Commands::default();
    state.world.step(event, &mut out);
    world.check_step(state);
    out.take_pacer()
}

pub(crate) struct PacerPort(Arc<Controller>);

impl PacerPort {
    pub(crate) fn open(lead_ms: u16, adaptive: bool) -> Self {
        let world = CURRENT.try_with(|(world, _)| world.clone()).unwrap_or_else(|_| {
            // Standalone component fixtures script their arrivals against the floor.
            if cfg!(test) {
                Controller::with_pacer(None, State::new(false))
            } else {
                Controller::new()
            }
        });
        let port = Self(world);
        port.send(PacerEvent::RunOpened { lead_ms, adaptive });
        port
    }

    pub(crate) fn dispatch(&self, event: &mut PacerEvent) -> PacerCommands {
        dispatch(&self.0, event)
    }

    pub(crate) fn send(&self, mut event: PacerEvent) -> PacerCommands {
        let mut out = self.dispatch(&mut event);
        if matches!(out.notification(), Some(Command::Pacer(PacerCommand::LipBatch)))
            && let Some(batch) = out.take_lips()
        {
            let taken = CURRENT
                .try_with(|(_, lips)| lips.as_ref().is_some_and(|lips| lips.send(&batch)))
                .unwrap_or(false);
            self.dispatch(&mut PacerEvent::LipDelivered { taken });
        }
        out
    }

    pub(crate) fn read<T>(&self, read: impl FnOnce(&State) -> T) -> T {
        read(&self.0.state().world.pacer)
    }

    pub(crate) fn take_lip_stats(&self) -> LipStats {
        let mut state = self.0.state();
        let stats = state.world.pacer.lip_stats();
        step(&self.0, &mut state, &mut Event::Pacer(PacerEvent::LipStatsCollected));
        stats
    }

    pub(crate) fn playout_due(&self) -> Option<Instant> {
        self.instant(self.read(Pacer::next_deadline))
    }

    pub(crate) fn mouth_change(&self) -> Option<bool> {
        let mut state = self.0.state();
        let mut out = Commands::default();
        state.world.tick_pacer(&mut out);
        self.0.check_step(&mut state);
        match out.take_pacer().notification() {
            Some(Command::Pacer(PacerCommand::MouthChanged(open))) => Some(open),
            _ => None,
        }
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
