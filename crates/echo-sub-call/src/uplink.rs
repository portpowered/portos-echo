use echo_core_api::{Broken, CallCommand, CallCommands, CallPcmFrame, SessionHandle, UplinkEvent, UplinkReceipt};

/// Thirty seconds of 20 ms frames, allocated once when World is constructed.
const CAPACITY: usize = 1500;

pub(super) struct Backlog {
    slots: Box<[CallPcmFrame]>,
    head: usize,
    depth: usize,
    run: Option<SessionHandle>,
    dropped: u64,
    next_attempt: u64,
    offered: Option<u64>,
}

impl Default for Backlog {
    fn default() -> Self {
        Self {
            slots: vec![[0; 320]; CAPACITY].into_boxed_slice(),
            head: 0,
            depth: 0,
            run: None,
            dropped: 0,
            next_attempt: 0,
            offered: None,
        }
    }
}

impl Backlog {
    pub(super) fn run(&self) -> Option<SessionHandle> {
        self.run
    }

    pub(super) fn depth(&self) -> usize {
        self.depth
    }

    pub(super) fn dropped(&self) -> u64 {
        self.dropped
    }

    pub(super) fn apply(&mut self, run: SessionHandle, event: UplinkEvent<'_>, out: &mut CallCommands) {
        if matches!(event, UplinkEvent::CaptureOpened) && self.run.is_none() {
            self.run = Some(run);
            self.dropped = 0;
        }
        if self.run != Some(run) {
            return;
        }
        match event {
            UplinkEvent::CaptureOpened => (),
            UplinkEvent::MicFrame(frame) => self.push(run, frame, out),
            UplinkEvent::Writable => self.offer(run, out),
            UplinkEvent::Result { attempt, receipt } if self.offered == Some(attempt) => {
                self.offered = None;
                if receipt == UplinkReceipt::Accepted {
                    self.pop();
                }
            }
            UplinkEvent::Result { .. } => (),
            UplinkEvent::DropBacklog | UplinkEvent::CaptureClosed => {
                self.head = 0;
                self.depth = 0;
                self.offered = None;
                if matches!(event, UplinkEvent::CaptureClosed) {
                    self.run = None;
                }
                out.emit(CallCommand::DropBacklog(run), None);
            }
        }
    }

    fn push(&mut self, run: SessionHandle, frame: &CallPcmFrame, out: &mut CallCommands) {
        let was_empty = self.depth == 0;
        if self.depth == CAPACITY {
            self.offered = None;
            self.pop();
            self.dropped += 1;
            out.uplink_evicted = true;
        }
        self.slots[(self.head + self.depth) % CAPACITY] = *frame;
        self.depth += 1;
        if was_empty {
            self.offer(run, out);
        }
    }

    fn offer(&mut self, run: SessionHandle, out: &mut CallCommands) {
        if self.depth == 0 || self.offered.is_some() {
            return;
        }
        let attempt = self.next_attempt;
        self.next_attempt = attempt.wrapping_add(1);
        self.offered = Some(attempt);
        out.emit(
            CallCommand::SendUplinkFrame { run, attempt },
            Some(&self.slots[self.head]),
        );
    }

    fn pop(&mut self) {
        self.depth -= 1;
        self.head = (self.head + 1) % CAPACITY;
    }

    pub(super) fn check(&self) -> Result<(), Broken> {
        if self.depth > CAPACITY {
            return Err(Broken {
                field: "call.uplink_depth",
                expected: "at most 1500 frames",
                actual: "backlog exceeds capacity",
            });
        }
        if (self.run.is_none() && self.depth != 0) || (self.depth == 0 && self.offered.is_some()) {
            return Err(Broken {
                field: "call.uplink_owner",
                expected: "an active run owns each pending frame and offer",
                actual: "unowned backlog or offer",
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
