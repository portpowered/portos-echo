//! Bounded trace admission. File effects belong to the drain service.
use echo_core_api::{Broken, CommandKind, Commands, Event, EventKind, Now, PendingTrace, Subsystem};
use std::{collections::VecDeque, time::Duration};

const CAPACITY: usize = 64;
pub const TICK: Duration = Duration::from_millis(100);

pub struct State {
    rows: VecDeque<PendingTrace>,
    open: bool,
    active_run: Option<String>,
    deadline: Option<Duration>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            rows: VecDeque::with_capacity(CAPACITY),
            open: true,
            active_run: None,
            deadline: None,
        }
    }
}

impl State {
    #[must_use]
    pub fn current_run(&self) -> Option<String> {
        self.active_run.clone()
    }

    pub fn set_run(&mut self, run: Option<&str>) {
        self.active_run = run.map(str::to_owned);
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    #[must_use]
    pub fn next_row(&mut self) -> Option<PendingTrace> {
        let row = self.rows.pop_front();
        if self.rows.is_empty() {
            self.deadline = None;
        }
        row
    }
}

pub struct Trace;

impl Subsystem for Trace {
    type State = State;
    const INPUTS: &'static [EventKind] = &[EventKind::Trace];
    const OUTPUTS: &'static [CommandKind] = &[CommandKind::DrainTrace];

    fn on_event(state: &mut State, event: &mut Event, now: Now, out: &mut Commands) {
        if let Event::Trace(row) = event
            && state.open
            && state.rows.len() < CAPACITY
            && let Some(row) = row.take()
        {
            if row.receipt.is_some() {
                out.drain_trace();
            }
            state.rows.push_back(row);
            state.deadline.get_or_insert(now.monotonic.saturating_add(TICK));
        }
    }

    fn on_tick(state: &mut State, _now: Now, out: &mut Commands) {
        if !state.rows.is_empty() {
            out.drain_trace();
        }
    }

    fn next_deadline(state: &State) -> Option<Duration> {
        state.deadline
    }

    fn check(state: &State) -> Result<(), Broken> {
        if state.rows.is_empty() != state.deadline.is_none() {
            return Err(Broken {
                field: "trace.deadline",
                expected: "present exactly while rows are pending",
                actual: "inconsistent",
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
