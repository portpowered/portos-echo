//! Bounded trace admission. File effects belong to the drain service.
use echo_core_api::{Broken, CommandKind, Commands, Event, EventKind, Now, PendingTrace, Subsystem};
use std::{collections::VecDeque, time::Duration};

const CAPACITY: usize = 64;
pub const TICK: Duration = Duration::from_millis(100);

pub struct State {
    rows: VecDeque<(u64, PendingTrace)>,
    open: bool,
    active_run: Option<String>,
    deadline: Option<Duration>,
    last_sequence: u64,
    ready_through: u64,
    in_flight: Option<u64>,
    writer_failed: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            rows: VecDeque::with_capacity(CAPACITY),
            open: true,
            active_run: None,
            deadline: None,
            last_sequence: 0,
            ready_through: 0,
            in_flight: None,
            writer_failed: false,
        }
    }
}

impl State {
    #[must_use]
    pub fn pending_rows(&self) -> usize {
        self.rows.len() + usize::from(self.in_flight.is_some())
    }

    #[must_use]
    pub fn writer_failed(&self) -> bool {
        self.writer_failed
    }

    #[must_use]
    pub fn current_run(&self) -> Option<String> {
        self.active_run.clone()
    }

    pub fn set_run(&mut self, run: Option<&str>) {
        self.active_run = run.map(str::to_owned);
    }

    pub fn close(&mut self) {
        self.open = false;
        self.release_rows();
    }

    #[must_use]
    pub fn next_row(&mut self) -> Option<(u64, PendingTrace)> {
        if self.in_flight.is_some() || self.rows.front()?.0 > self.ready_through {
            return None;
        }
        let (sequence, row) = self.rows.pop_front()?;
        self.in_flight = Some(sequence);
        Some((sequence, row))
    }

    fn release_rows(&mut self) {
        self.ready_through = self.last_sequence;
        self.deadline = None;
    }

    fn admit(&mut self, row: &mut Option<PendingTrace>, now: Now, out: &mut Commands) {
        if !self.open || self.pending_rows() == CAPACITY {
            return;
        }
        if let Some(row) = row.take() {
            let checked = row.receipt.is_some();
            self.last_sequence += 1;
            self.rows.push_back((self.last_sequence, row));
            if checked {
                self.release_rows();
                out.drain_trace();
            } else {
                self.deadline.get_or_insert(now.monotonic.saturating_add(TICK));
            }
        }
    }
}

pub struct Trace;

impl Subsystem for Trace {
    type State = State;
    const INPUTS: &'static [EventKind] = &[EventKind::TraceRow, EventKind::TraceFlushed, EventKind::TraceFailed];
    const OUTPUTS: &'static [CommandKind] = &[CommandKind::DrainTrace];

    fn on_event(state: &mut State, event: &mut Event, now: Now, out: &mut Commands) {
        match event {
            Event::TraceRow(row) => state.admit(row, now, out),
            Event::TraceFlushed { through } if state.in_flight == Some(*through) => state.in_flight = None,
            Event::TraceFailed => {
                state.in_flight = None;
                state.writer_failed = true;
            }
            _ => {}
        }
    }

    fn on_tick(state: &mut State, now: Now, out: &mut Commands) {
        if state.deadline.is_some_and(|deadline| deadline <= now.monotonic) {
            state.release_rows();
            out.drain_trace();
        }
    }

    fn next_deadline(state: &State) -> Option<Duration> {
        state.deadline
    }

    fn check(state: &State) -> Result<(), Broken> {
        if (state.ready_through < state.last_sequence) != state.deadline.is_some() {
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
