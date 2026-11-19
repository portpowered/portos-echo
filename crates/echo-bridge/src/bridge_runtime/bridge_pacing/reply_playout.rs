//! Audio-clock playout of a reply that arrives in real time (the WebRTC peer).
use echo_audio::FrameQueue;
use std::time::Duration;
use tokio::time::Instant;

pub(crate) const PERIOD: Duration = Duration::from_millis(20);
/// Samples at or below this level are WebRTC silence, not speech.
pub(crate) const QUIET: u16 = 64;

/// The cushion a dry Echo gets before playback starts. Frames from the call
/// arrive in real time with jitter (measured on the Echo: gaps up to 62 ms
/// between 20 ms frames, `docs/program/baselines.md`); forwarded one by one
/// into an empty player, every late frame is an audible underrun.
const PRIME: Duration = super::reply_jitter::FLOOR;
/// Delay this far above the target is left alone, so the drain never hunts.
const SLACK: Duration = Duration::from_millis(40);

/// What the pacer needs to know of a frame to end and resume audio cleanly.
pub(crate) trait Pcm: Clone {
    /// The frame holds more than silence.
    fn audible(&self) -> bool;
    /// This frame played backwards while fading to silence: it continues the
    /// waveform from the frame's last sample, so the speaker does not click.
    #[must_use]
    fn concealment(&self) -> Self;
    /// Ramp this frame up from silence.
    fn fade_in(&mut self);
}

/// Signed 16-bit little-endian mono, as written to the Echo.
impl Pcm for Vec<u8> {
    fn audible(&self) -> bool {
        let (samples, _) = self.as_chunks::<2>();
        samples
            .iter()
            .any(|sample| i16::from_le_bytes(*sample).unsigned_abs() > QUIET)
    }

    fn concealment(&self) -> Self {
        let (samples, _) = self.as_chunks::<2>();
        let whole = u32::try_from(samples.len()).unwrap_or(u32::MAX);
        let ramp = (1..=whole).rev();
        samples
            .iter()
            .rev()
            .zip(ramp)
            .flat_map(|(sample, part)| scaled(*sample, part, whole))
            .collect()
    }

    fn fade_in(&mut self) {
        let (samples, _) = self.as_chunks_mut::<2>();
        let whole = u32::try_from(samples.len()).unwrap_or(u32::MAX);
        for (sample, part) in samples.iter_mut().zip(0..whole) {
            *sample = scaled(*sample, part, whole);
        }
    }
}

fn scaled(sample: [u8; 2], part: u32, whole: u32) -> [u8; 2] {
    let value = i64::from(i16::from_le_bytes(sample)) * i64::from(part) / i64::from(whole.max(1));
    i16::try_from(value).unwrap_or_default().to_le_bytes()
}

/// What the pacer did to keep one reply's audio whole.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Playout {
    /// Starts after the Echo had played out everything sent, mid-reply.
    pub reprimes: u32,
    /// Fade-out frames written just before the Echo ran dry.
    pub concealed: u32,
    /// Frames ramped up from silence after such a fade-out.
    pub fade_ins: u32,
    /// Silent frames left out to bring the delay back to the target.
    pub drained: u32,
}

/// Audio-clock pacing for a reply that arrives in real time (the WebRTC peer).
/// A dry device first gets the target of audio at once (the lead's worth on
/// the device, the rest held here): the pacer waits for that much to gather,
/// for the end of the reply, or the target from when the frame was first
/// offered, whichever is first, so a burst is never delayed and no frame is
/// held longer than the target. The target is 100 ms (at most the lead) until
/// `retarget` raises it for measured lateness.
/// After that frames go out as they arrive, never more than `lead` ahead of
/// the clock: frame k is due at `start + 20 ms * (k + 1) - lead`. Late
/// wakeups (a coarse OS timer) catch up instead of stretching every frame. Once
/// the device has played out everything sent, the clock restarts the same way.
///
/// A reply that stalls mid-speech does not cut off: one period before the
/// device runs dry it gets the last frame mirrored and faded to silence, and
/// the first frame after the gap fades in. Delay gathered above the target is
/// given back by leaving out silent frames. Under the sim's stepped clock the
/// pacer does none of this and holds nothing.
pub(crate) struct ClockPacer<T> {
    lead_frames: u32,
    adaptive: bool,
    floor: Duration,
    target: Duration,
    /// When the frame at the head of the queue was first offered.
    offered: Option<Instant>,
    started: Option<Instant>,
    sent: u32,
    /// Frames of delay the last start gathered that may be given back.
    cushion: u32,
    fade_in: bool,
    /// The last frame sent, while it is audible and the reply goes on.
    tail: Option<T>,
    playout: Playout,
}

fn frames(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis() / PERIOD.as_millis()).unwrap_or(u32::MAX)
}

impl<T: Pcm> ClockPacer<T> {
    pub(crate) fn new(lead_ms: u16) -> Self {
        let lead_frames = u32::from(lead_ms / 20);
        // The sim's stepped clock stands still while a released reply plays
        // on the device's wall clock: a timed hold would never end there.
        let adaptive = crate::bridge_mcp::bridge_clock::receipt::Context::current().is_none();
        let floor = PRIME.min(PERIOD * if adaptive { lead_frames } else { 0 });
        Self {
            lead_frames,
            adaptive,
            floor,
            target: floor,
            offered: None,
            started: None,
            sent: 0,
            cushion: 0,
            fade_in: false,
            tail: None,
            playout: Playout::default(),
        }
    }

    /// Follow the jitter estimate; returns the target in force. A call with
    /// no lateness to cover keeps the lead-bounded floor.
    pub(crate) fn retarget(&mut self, wanted: Duration) -> Duration {
        if self.adaptive {
            self.target = if wanted > PRIME { wanted } else { self.floor };
        }
        self.target
    }

    pub(crate) fn take_playout(&mut self) -> Playout {
        std::mem::take(&mut self.playout)
    }

    /// With nothing queued, the device is owed a fade-out if the reply stalls.
    pub(crate) fn conceals(&self, flush: bool) -> bool {
        !flush && self.started.is_some() && self.tail.is_some()
    }

    /// Cancellation while waiting consumes no frame and no clock credit.
    /// `flush`: no more frames will follow, so nothing is worth waiting for.
    pub(crate) async fn next_frame(&mut self, queue: &mut FrameQueue<T>, flush: bool) -> Option<T> {
        if queue.is_empty() {
            return None;
        }
        let mut now = Instant::now();
        // The hold survives cancelled polls: it runs from the first offer.
        let release = *self.offered.get_or_insert(now) + self.target;
        let started = match self.started {
            Some(started) if started + PERIOD * self.sent >= now => started,
            dry => {
                let gathered = PERIOD * u32::try_from(queue.len()).unwrap_or(u32::MAX);
                if !flush && gathered < self.target && release > now {
                    tokio::time::sleep_until(release).await;
                    now = Instant::now();
                }
                self.restart(dry.is_some(), queue.len(), now)
            }
        };
        let due = started + (PERIOD * (self.sent + 1)).saturating_sub(PERIOD * self.lead_frames);
        // Timers round up to the next millisecond: never sleep for a frame already due.
        if due > now {
            tokio::time::sleep_until(due).await;
        }
        self.offered = None;
        self.send(queue, flush)
    }

    /// The device is dry: its clock starts at `now` with `queued` frames.
    fn restart(&mut self, resumed: bool, queued: usize, now: Instant) -> Instant {
        let queued = u32::try_from(queued).unwrap_or(u32::MAX);
        self.sent = 0;
        if resumed {
            // Everything a stall let gather is delay the reply now carries.
            self.cushion = queued;
            self.playout.reprimes += 1;
            self.tail = None;
        } else {
            // A source faster than real time is not delay: only the cushion
            // asked for is ever given back.
            self.cushion = queued.min(frames(self.target));
        }
        *self.started.insert(now)
    }

    fn send(&mut self, queue: &mut FrameQueue<T>, flush: bool) -> Option<T> {
        loop {
            let mut frame = queue.pop()?;
            let audible = self.adaptive && frame.audible();
            if !audible && self.excess(queue.len() + 1, flush) {
                self.cushion -= 1;
                self.playout.drained += 1;
                continue;
            }
            self.sent += 1;
            if self.adaptive {
                if std::mem::take(&mut self.fade_in) && audible {
                    frame.fade_in();
                    self.playout.fade_ins += 1;
                }
                self.tail = audible.then(|| frame.clone());
            }
            return Some(frame);
        }
    }

    /// The reply carries more delay than the target asks for: both what the
    /// last start gathered and what is unplayed right now say so.
    fn excess(&self, queued: usize, flush: bool) -> bool {
        let Some(started) = self.started.filter(|_| self.adaptive && !flush) else {
            return false;
        };
        let over = self.target + SLACK;
        let unplayed = (started + PERIOD * self.sent).saturating_duration_since(Instant::now())
            + PERIOD * u32::try_from(queued).unwrap_or(u32::MAX);
        PERIOD * self.cushion > over && unplayed > over
    }

    /// For when nothing is queued. If the reply was mid-speech, wait until the
    /// device has one period left and hand it a fade-out instead of a cut.
    /// Cancellation while waiting changes nothing.
    pub(crate) async fn fade_out(&mut self, flush: bool) -> Option<T> {
        if !self.conceals(flush) {
            return None;
        }
        let end = self.started? + PERIOD * self.sent;
        if let Some(last_period) = end.checked_sub(PERIOD) {
            tokio::time::sleep_until(last_period).await;
        }
        let tail = self.tail.take()?;
        // Already dry: a fade-out now would play after the cut it is for.
        if end < Instant::now() {
            return None;
        }
        self.fade_in = true;
        self.sent += 1;
        self.playout.concealed += 1;
        Some(tail.concealment())
    }
}

#[cfg(test)]
#[path = "playout_tests.rs"]
mod tests;
