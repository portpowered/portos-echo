//! Reply pacing, using the session clock and emitting commands for its services.
mod lips;
mod mouth;
mod reply_ear;
mod reply_jitter;
mod reply_playout;
mod ring;

use echo_core_api::{
    Broken, CommandKind, Commands, Event, EventKind, Now, PacerCommands, PacerEvent, PacerInput, PacerOutput,
    PacerStatus, PacerView, REPLY_CAPACITY, ReplyFrame, Subsystem,
};
use lips::Lips;
use mouth::Mouth;
use reply_ear::Ear;
use reply_jitter::{EXPECTED_MS, Jitter};
use reply_playout::{ClockPacer, Wait};
use ring::Ring;
use std::time::Duration;

pub struct Pacer;

pub struct State {
    queue: Ring,
    clock: ClockPacer,
    wait: Option<Wait>,
    staged: Option<ReplyFrame>,
    jitter: Jitter,
    ear: Ear,
    mouth: Mouth,
    lips: Option<Lips>,
    lead_ms: u16,
    adaptive: bool,
    input: PacerInput,
    spoken: bool,
    last_frame_at: Option<Duration>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            queue: Ring::default(),
            clock: ClockPacer::new(0, false),
            wait: None,
            staged: None,
            jitter: Jitter::default(),
            ear: Ear::default(),
            mouth: Mouth::default(),
            lips: None,
            lead_ms: 0,
            adaptive: false,
            input: PacerInput::Closed,
            spoken: false,
            last_frame_at: None,
        }
    }
}

impl State {
    #[must_use]
    pub fn status(&self) -> PacerStatus {
        let depth = self.queue.len();
        let output = if self.staged.is_some() {
            PacerOutput::Staged
        } else if depth == 0 && self.clock.conceals(self.input == PacerInput::Finished) {
            PacerOutput::Stalled
        } else {
            PacerOutput::Idle
        };
        PacerStatus {
            view: PacerView {
                depth,
                playing: self.mouth.voiced(),
                last_frame_at: self.last_frame_at,
            },
            input: self.input,
            output,
            spoken: self.spoken,
            sounded: self.ear.sounded(),
            dropped: self.queue.dropped(),
            deadline: Pacer::next_deadline(self),
            mouth_due: self.mouth.due(),
        }
    }

    #[must_use]
    pub fn quiescent(&self) -> bool {
        self.input == PacerInput::Closed
            && self.queue.len() == 0
            && self.wait.is_none()
            && self.staged.is_none()
            && !self.mouth.voiced()
            && !self.lips.as_ref().is_some_and(Lips::has_pending)
    }

    fn reset_reply(&mut self, out: &mut PacerCommands) {
        out.playout = self.clock.take_playout();
        self.queue.clear();
        self.clock = ClockPacer::new(self.lead_ms, self.adaptive);
        self.wait = None;
        self.staged = None;
        self.input = PacerInput::Open;
        self.spoken = false;
        self.last_frame_at = None;
        self.mouth = Mouth::default();
    }

    fn close_run(&mut self, out: &mut PacerCommands) {
        self.reset_reply(out);
        self.input = PacerInput::Closed;
        self.ear = Ear::default();
        self.jitter = Jitter::default();
        if let Some(lips) = &mut self.lips {
            // Discard an abandoned run's pending samples without sending stale
            // audio metadata. Preserve the screen's epoch across device runs.
            lips.abandon();
        }
    }

    fn offer(&mut self, allow_fade: bool, now: Duration, out: &mut PacerCommands) {
        if self.staged.is_none() {
            self.wait = self
                .clock
                .offer(self.queue.len(), self.input == PacerInput::Finished, allow_fade, now);
            self.tick(now, out);
        }
    }

    fn tick(&mut self, now: Duration, out: &mut PacerCommands) {
        let flush = self.input == PacerInput::Finished;
        if let Some(frame) = self.clock.tick(&mut self.wait, &mut self.queue, flush, now) {
            self.staged = Some(frame);
            out.frame = Some(frame);
        }
    }

    fn handed(&mut self, now: Duration, wanted: Option<bool>, out: &mut PacerCommands) {
        if let Some(frame) = self.staged.take() {
            self.last_frame_at = Some(now);
            self.ear.played(&frame, now);
            let start = self.mouth.wrote(&frame, now);
            out.lips = self
                .lips
                .as_mut()
                .and_then(|lips| lips.wrote(&frame, start, now, wanted));
        }
    }

    fn settle(&mut self, now: Duration, out: &mut PacerCommands) {
        self.spoken = false;
        self.jitter.rest();
        out.playout = self.clock.take_playout();
        out.heard = self.ear.rest();
        out.lips = self.lips.as_mut().and_then(|lips| lips.settle(now));
    }
}

impl Subsystem for Pacer {
    type State = State;
    const INPUTS: &'static [EventKind] = &[EventKind::Pacer];
    const OUTPUTS: &'static [CommandKind] = &[
        CommandKind::WriteFrame,
        CommandKind::LipBatch,
        CommandKind::MouthChanged,
        CommandKind::PlayoutFinished,
    ];

    fn on_event(state: &mut State, event: &mut Event, now: Now, out: &mut Commands) {
        let Event::Pacer(event) = event else { return };
        let now = now.monotonic;
        let out = &mut out.pacer;
        match event {
            PacerEvent::ConfigureLips { delay } => state.lips = delay.map(|delay| Lips::new(delay, now)),
            PacerEvent::RunOpened { lead_ms, adaptive } => {
                state.lead_ms = *lead_ms;
                state.adaptive = *adaptive;
                state.close_run(out);
                state.input = PacerInput::Open;
            }
            PacerEvent::ExpectJitter => state.jitter = Jitter::expecting(EXPECTED_MS),
            PacerEvent::ReplyOpened => state.reset_reply(out),
            PacerEvent::PeerFrameArrived { audible } => {
                state.spoken |= *audible;
                state.jitter.arrived(now);
                out.target = Some(state.clock.retarget(state.jitter.target(now)));
            }
            PacerEvent::PeerFrameDiscarded => state.jitter.rest(),
            PacerEvent::PcmPushed(frame) => state.queue.push(frame),
            PacerEvent::MicrophoneFrame(frame) => {
                state.ear.heard(frame.as_slice(), now);
            }
            PacerEvent::InputFinished => state.input = PacerInput::Finished,
            PacerEvent::Offer { allow_fade } => state.offer(*allow_fade, now, out),
            PacerEvent::Tick => state.tick(now, out),
            PacerEvent::FrameHanded { screen_wanted } => state.handed(now, *screen_wanted, out),
            PacerEvent::LipDelivered { taken } => {
                if let Some(lips) = &mut state.lips {
                    lips.delivered(*taken);
                }
            }
            PacerEvent::MouthSettled => state.mouth.settle(),
            PacerEvent::MouthTick => out.mouth_changed = state.mouth.poll(now),
            PacerEvent::Played => {
                out.finished = matches!(state.input, PacerInput::Finished | PacerInput::Cut)
                    && state.queue.len() == 0
                    && state.staged.is_none()
            }
            PacerEvent::Cut => {
                state.reset_reply(out);
                state.input = PacerInput::Cut;
                out.lips = state.lips.as_mut().and_then(|lips| lips.cut(now));
            }
            PacerEvent::ReplySettled => state.settle(now, out),
            PacerEvent::TakeLipStats => {
                if let Some(lips) = &mut state.lips {
                    out.lip_stats = lips.take_stats();
                }
            }
            PacerEvent::RunClosed => state.close_run(out),
        }
    }

    fn next_deadline(state: &State) -> Option<Duration> {
        state.wait.map(Wait::due)
    }

    fn check(state: &State) -> Result<(), Broken> {
        if state.queue.len() > REPLY_CAPACITY {
            return Err(Broken {
                field: "pacer.reply_depth",
                expected: "0..=128",
                actual: "out of range",
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
