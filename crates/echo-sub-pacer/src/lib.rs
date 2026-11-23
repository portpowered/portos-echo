//! Reply pacing, using the session clock and emitting commands for its services.
mod lips;
mod mouth;
mod reply_ear;
mod reply_jitter;
mod reply_playout;
mod ring;

use echo_core_api::{
    Broken, CommandKind, Commands, Event, EventKind, Now, PacerCommands, PacerEvent, PacerView, ReplyFrame, Subsystem,
};
pub use lips::LipStats;
use lips::Lips;
use mouth::Mouth;
use reply_ear::Ear;
pub use reply_ear::Heard;
use reply_jitter::{EXPECTED_MS, Jitter};
pub use reply_playout::Playout;
use reply_playout::{ClockPacer, Wait};
use ring::Ring;
use std::time::Duration;

const REPLY_CAPACITY: usize = 128;
pub const PERIOD: Duration = Duration::from_millis(20);
/// Samples at or below this level count as WebRTC silence.
pub const QUIET: u16 = 64;

pub struct Pacer;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Input {
    Closed,
    Open,
    Finished,
    Cut,
}

pub struct State {
    queue: Ring,
    clock: ClockPacer,
    wait: Option<Wait>,
    staged: Option<ReplyFrame>,
    jitter: Jitter,
    prime_first_arrival: bool,
    ear: Ear,
    mouth: Mouth,
    lips: Option<Lips>,
    lead_ms: u16,
    adaptive: bool,
    input: Input,
    spoken: bool,
    last_frame_at: Option<Duration>,
}

impl Default for State {
    fn default() -> Self {
        Self::new(true)
    }
}

impl State {
    /// Construct a pacer with the given first-arrival jitter policy.
    #[must_use]
    pub fn new(prime_first_arrival: bool) -> Self {
        Self {
            queue: Ring::default(),
            clock: ClockPacer::new(0, false),
            wait: None,
            staged: None,
            jitter: Jitter::default(),
            prime_first_arrival,
            ear: Ear::default(),
            mouth: Mouth::default(),
            lips: None,
            lead_ms: 0,
            adaptive: false,
            input: Input::Closed,
            spoken: false,
            last_frame_at: None,
        }
    }

    #[must_use]
    pub fn view(&self) -> PacerView {
        PacerView {
            depth: self.queue.len(),
            playing: self.mouth.voiced(),
            last_frame_at: self.last_frame_at,
        }
    }

    #[must_use]
    pub fn stalled(&self) -> bool {
        self.staged.is_none() && self.queue.len() == 0 && self.clock.conceals(self.finished())
    }

    #[must_use]
    pub fn finished(&self) -> bool {
        self.input == Input::Finished
    }

    #[must_use]
    pub fn drained(&self) -> bool {
        self.finished() && self.queue.len() == 0 && self.staged.is_none()
    }

    #[must_use]
    pub fn spoken(&self) -> bool {
        self.spoken
    }

    #[must_use]
    pub fn sounded(&self) -> bool {
        self.ear.sounded()
    }

    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.queue.dropped()
    }

    #[must_use]
    pub fn mouth_due(&self) -> Option<Duration> {
        self.mouth.due()
    }

    #[must_use]
    pub fn target(&self) -> Duration {
        self.clock.target()
    }

    #[must_use]
    pub fn playout(&self) -> Playout {
        self.clock.playout()
    }

    #[must_use]
    pub fn heard(&self) -> Heard {
        self.ear.measurement()
    }

    #[must_use]
    pub fn lip_stats(&self) -> LipStats {
        self.lips.as_ref().map_or_else(LipStats::default, Lips::stats)
    }

    /// Retire unsent PCM on remote end. The voice owner still settles the lip window.
    fn discard_output(&mut self) {
        self.queue.discard();
        self.wait = None;
        self.staged = None;
        self.clock.discard_tail();
    }

    #[must_use]
    pub fn quiescent(&self) -> bool {
        self.input == Input::Closed
            && self.queue.len() == 0
            && self.wait.is_none()
            && self.staged.is_none()
            && !self.mouth.voiced()
            && !self.lips.as_ref().is_some_and(Lips::has_pending)
    }

    fn reset_reply(&mut self) {
        self.queue.clear();
        self.clock = ClockPacer::new(self.lead_ms, self.adaptive);
        self.wait = None;
        self.staged = None;
        self.input = Input::Open;
        self.spoken = false;
        self.last_frame_at = None;
        self.mouth.reset();
    }

    fn close_run(&mut self) {
        self.reset_reply();
        self.input = Input::Closed;
        self.ear = Ear::default();
        self.jitter = Jitter::default();
        if let Some(lips) = &mut self.lips {
            // Discard an abandoned run's pending samples without sending stale
            // audio metadata. Preserve the screen's epoch across device runs.
            lips.abandon();
        }
    }

    fn open_run(&mut self, lead_ms: u16, adaptive: bool) {
        self.lead_ms = lead_ms;
        self.adaptive = adaptive;
        self.close_run();
        if self.prime_first_arrival {
            // The first arrival starts with the cushion measured on the Echo.
            self.jitter = Jitter::expecting(EXPECTED_MS);
        }
        self.input = Input::Open;
    }

    fn offer(&mut self, allow_fade: bool, now: Duration, out: &mut PacerCommands) {
        if self.staged.is_none() {
            self.wait = self.clock.offer(self.queue.len(), self.finished(), allow_fade, now);
            self.tick(now, out);
        }
    }

    fn tick(&mut self, now: Duration, out: &mut PacerCommands) {
        let flush = self.finished();
        if let Some(frame) = self.clock.tick(&mut self.wait, &mut self.queue, flush, now) {
            self.staged = Some(frame);
            out.write_frame(&frame);
        }
    }

    fn handed(&mut self, now: Duration, wanted: Option<bool>, out: &mut PacerCommands) {
        if let Some(frame) = self.staged.take() {
            self.last_frame_at = Some(now);
            self.ear.played(&frame, now);
            let start = self.mouth.wrote(&frame, now);
            out.publish_lips(
                self.lips
                    .as_mut()
                    .and_then(|lips| lips.wrote(&frame, start, now, wanted)),
            );
        }
    }

    fn settle(&mut self, now: Duration, out: &mut PacerCommands) {
        self.spoken = false;
        self.jitter.rest();
        self.clock.take_playout();
        self.ear.rest();
        out.publish_lips(self.lips.as_mut().and_then(|lips| lips.settle(now)));
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
        let out = out.pacer();
        match event {
            PacerEvent::ConfigureLips { delay } => state.lips = delay.map(|delay| Lips::new(delay, now)),
            PacerEvent::RunOpened { lead_ms, adaptive } => state.open_run(*lead_ms, *adaptive),
            PacerEvent::ReplyOpened => state.reset_reply(),
            PacerEvent::PeerFrameArrived { audible } => {
                state.spoken |= *audible;
                state.jitter.arrived(now);
                state.clock.retarget(state.jitter.target(now));
            }
            PacerEvent::PeerFrameDiscarded => state.jitter.rest(),
            PacerEvent::PcmPushed(frame) => state.queue.push(frame),
            PacerEvent::MicrophoneFrame(frame) => {
                state.ear.heard(frame.as_slice(), now);
            }
            PacerEvent::InputFinished => state.input = Input::Finished,
            PacerEvent::Offer { allow_fade } => state.offer(*allow_fade, now, out),
            PacerEvent::Tick => state.tick(now, out),
            PacerEvent::FrameHanded { screen_wanted } => state.handed(now, *screen_wanted, out),
            PacerEvent::LipDelivered { taken } => {
                if let Some(lips) = &mut state.lips {
                    lips.delivered(*taken);
                }
            }
            PacerEvent::MouthSettled => state.mouth.settle(),
            PacerEvent::Played => {
                if matches!(state.input, Input::Finished | Input::Cut)
                    && state.queue.len() == 0
                    && state.staged.is_none()
                {
                    out.finish_playout();
                }
            }
            PacerEvent::Cut => {
                state.reset_reply();
                state.input = Input::Cut;
                out.publish_lips(state.lips.as_mut().and_then(|lips| lips.cut(now)));
            }
            PacerEvent::RemoteEnd => state.discard_output(),
            PacerEvent::ReplySettled => state.settle(now, out),
            PacerEvent::LipStatsCollected => {
                if let Some(lips) = &mut state.lips {
                    lips.clear_stats();
                }
            }
            PacerEvent::RunClosed => state.close_run(),
        }
    }

    // Mouth notices have their own wake in the voice loop. The explicit Tick
    // event resolves an outstanding audio offer and can stage PCM instead.
    fn on_tick(state: &mut State, now: Now, out: &mut Commands) {
        out.pacer().change_mouth(state.mouth.poll(now.monotonic));
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
