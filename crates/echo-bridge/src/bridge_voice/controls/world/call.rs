use super::{Controller, State, pacer};
use echo_core_api::{CallCommands, CallEvent, CallHandle, CallPhase, CallView, Commands, Event};
use std::sync::Arc;

pub(crate) struct CallPort {
    world: Arc<Controller>,
    handle: Option<CallHandle>,
}

impl CallPort {
    pub(crate) fn attach() -> Self {
        let world = pacer::current_controller().unwrap_or_else(Controller::new);
        let handle = {
            let mut state = world.state();
            let before = state.world.pacer.call.view().handle;
            step(&world, &mut state, CallEvent::PeerAttached);
            state
                .world
                .pacer
                .call
                .view()
                .handle
                .filter(|handle| Some(*handle) != before)
        };
        Self { world, handle }
    }

    pub(crate) fn view(&self) -> Option<CallView> {
        let handle = self.handle?;
        let view = self.world.state().world.pacer.call.view();
        Some(if view.handle == Some(handle) {
            view
        } else {
            // A new handle is only admitted after the previous peer closes.
            CallView {
                phase: CallPhase::Closed,
                ..CallView::default()
            }
        })
    }

    pub(crate) fn send<'a>(&self, event: impl FnOnce(CallHandle) -> CallEvent<'a>) -> CallCommands {
        self.handle.map_or_else(CallCommands::default, |call| {
            let event = event(call);
            step(&self.world, &mut self.world.state(), event)
        })
    }
}

fn step(world: &Controller, state: &mut State, event: CallEvent<'_>) -> CallCommands {
    let mut out = Commands::default();
    state.world.step(&mut Event::Call(event), &mut out);
    world.check_step(state);
    out.take_call()
}
