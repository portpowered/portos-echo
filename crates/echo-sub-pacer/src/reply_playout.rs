//! Audio-clock playout of a reply that arrives in real time (the WebRTC peer).
use super::ring::Ring;
use echo_core_api::{Playout, REPLY_PERIOD as PERIOD, REPLY_QUIET as QUIET, ReplyFrame};
use std::time::Duration;

/// The cushion a dry Echo gets before playback starts. Frames from the call
/// arrive in real time with jitter (measured on the Echo: gaps up to 62 ms
/// between 20 ms frames, `docs/program/baselines.md`); forwarded one by one
/// into an empty player, every late frame is an audible underrun.
const PRIME: Duration = super::reply_jitter::FLOOR;
/// Delay this far above the target is left alone, so the drain never hunts.
const SLACK: Duration = Duration::from_millis(40);

fn audible(frame: &ReplyFrame) -> bool {
    frame
        .as_chunks::<2>()
        .0
        .iter()
        .any(|sample| i16::from_le_bytes(*sample).unsigned_abs() > QUIET)
}

fn concealment(frame: &ReplyFrame) -> ReplyFrame {
    let mut faded = [0; 960];
    for (index, (input, output)) in frame
        .as_chunks::<2>()
        .0
        .iter()
        .rev()
        .zip(faded.as_chunks_mut::<2>().0)
        .enumerate()
    {
        *output = scaled(*input, 480 - u32::try_from(index).unwrap_or(480), 480);
    }
    faded
}

fn fade_in(frame: &mut ReplyFrame) {
    for (sample, part) in frame.as_chunks_mut::<2>().0.iter_mut().zip(0..480) {
        *sample = scaled(*sample, part, 480);
    }
}

fn scaled(sample: [u8; 2], part: u32, whole: u32) -> [u8; 2] {
    let value = i64::from(i16::from_le_bytes(sample)) * i64::from(part) / i64::from(whole.max(1));
    i16::try_from(value).unwrap_or_default().to_le_bytes()
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
pub(crate) struct ClockPacer {
    lead_frames: u32,
    adaptive: bool,
    floor: Duration,
    target: Duration,
    /// When the frame at the head of the queue was first offered.
    offered: Option<Duration>,
    started: Option<Duration>,
    sent: u32,
    /// Frames of delay the last start gathered that may be given back.
    cushion: u32,
    fade_in: bool,
    /// The last frame sent, while it is audible and the reply goes on.
    tail: Option<ReplyFrame>,
    playout: Playout,
}

fn frames(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis() / PERIOD.as_millis()).unwrap_or(u32::MAX)
}

impl ClockPacer {
    pub(crate) fn new(lead_ms: u16, adaptive: bool) -> Self {
        let lead_frames = u32::from(lead_ms / 20);
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

    /// Recompute from the head after another branch cancelled the previous offer.
    pub(crate) fn offer(&mut self, queued: usize, flush: bool, allow_fade: bool, now: Duration) -> Option<Wait> {
        if queued == 0 {
            let end = self.started? + PERIOD * self.sent;
            return (allow_fade && self.conceals(flush)).then_some(Wait::Fade {
                due: end.saturating_sub(PERIOD),
                end,
            });
        }
        let release = *self.offered.get_or_insert(now) + self.target;
        let started = match self.started {
            Some(started) if started + PERIOD * self.sent >= now => started,
            dry => {
                let gathered = PERIOD * u32::try_from(queued).unwrap_or(u32::MAX);
                if !flush && gathered < self.target && release > now {
                    return Some(Wait::Prime {
                        due: release,
                        resumed: dry.is_some(),
                    });
                }
                self.restart(dry.is_some(), queued, now)
            }
        };
        Some(self.release(started))
    }

    fn release(&self, started: Duration) -> Wait {
        let due = started + (PERIOD * (self.sent + 1)).saturating_sub(PERIOD * self.lead_frames);
        Wait::Release { due }
    }

    pub(crate) fn tick(
        &mut self,
        wait: &mut Option<Wait>,
        queue: &mut Ring,
        flush: bool,
        now: Duration,
    ) -> Option<ReplyFrame> {
        let waiting = (*wait).filter(|waiting| waiting.due() <= now)?;
        let waiting = match waiting {
            Wait::Prime { resumed, .. } => {
                let started = self.restart(resumed, queue.len(), now);
                let release = self.release(started);
                if release.due() > now {
                    *wait = Some(release);
                    return None;
                }
                release
            }
            other => other,
        };
        *wait = None;
        if let Wait::Fade { end, .. } = waiting {
            return self.fade_out(end, now);
        }
        self.offered = None;
        self.send(queue, flush, now)
    }

    /// The device is dry: its clock starts at `now` with `queued` frames.
    fn restart(&mut self, resumed: bool, queued: usize, now: Duration) -> Duration {
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

    fn send(&mut self, queue: &mut Ring, flush: bool, now: Duration) -> Option<ReplyFrame> {
        loop {
            let mut frame = queue.pop()?;
            let audible = self.adaptive && audible(&frame);
            if !audible && self.excess(queue.len() + 1, flush, now) {
                self.cushion -= 1;
                self.playout.drained += 1;
                continue;
            }
            self.sent += 1;
            if self.adaptive {
                if std::mem::take(&mut self.fade_in) && audible {
                    fade_in(&mut frame);
                    self.playout.fade_ins += 1;
                }
                self.tail = audible.then_some(frame);
            }
            return Some(frame);
        }
    }

    /// The reply carries more delay than the target asks for: both what the
    /// last start gathered and what is unplayed right now say so.
    fn excess(&self, queued: usize, flush: bool, now: Duration) -> bool {
        let Some(started) = self.started.filter(|_| self.adaptive && !flush) else {
            return false;
        };
        let over = self.target + SLACK;
        let unplayed =
            (started + PERIOD * self.sent).saturating_sub(now) + PERIOD * u32::try_from(queued).unwrap_or(u32::MAX);
        PERIOD * self.cushion > over && unplayed > over
    }

    fn fade_out(&mut self, end: Duration, now: Duration) -> Option<ReplyFrame> {
        let tail = self.tail.take()?;
        // Already dry: a fade-out now would play after the cut it is for.
        if end < now {
            return None;
        }
        self.fade_in = true;
        self.sent += 1;
        self.playout.concealed += 1;
        Some(concealment(&tail))
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Wait {
    Prime { due: Duration, resumed: bool },
    Release { due: Duration },
    Fade { due: Duration, end: Duration },
}

impl Wait {
    pub(crate) fn due(self) -> Duration {
        match self {
            Self::Prime { due, .. } | Self::Release { due } | Self::Fade { due, .. } => due,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(level: i16) -> ReplyFrame {
        let mut frame = [0; 960];
        frame.as_chunks_mut::<2>().0.fill(level.to_le_bytes());
        frame
    }

    fn sample(frame: &ReplyFrame, index: usize) -> i16 {
        i16::from_le_bytes(frame.as_chunks::<2>().0[index])
    }

    #[test]
    fn pacer_playout_fade_preserves_waveform_and_silence_threshold() {
        let mut ramp = [0; 960];
        for (out, sample) in ramp.as_chunks_mut::<2>().0.iter_mut().zip(0..480_i16) {
            *out = (sample * 50).to_le_bytes();
        }
        assert!(audible(&ramp) && !audible(&pcm(64)) && audible(&pcm(-65)));
        let faded = concealment(&ramp);
        assert_eq!(
            [0, 1, 240, 479].map(|index| sample(&faded, index)),
            [23950, 23850, 5975, 0]
        );
        assert_eq!(sample(&concealment(&pcm(-8000)), 479), -16);
        assert_eq!(concealment(&pcm(0)), pcm(0));

        let mut risen = pcm(8000);
        fade_in(&mut risen);
        assert_eq!([0, 1, 240, 479].map(|index| sample(&risen, index)), [0, 16, 4000, 7983]);
    }

    #[test]
    fn pacer_playout_fades_only_unfinished_audible_audio() {
        let mut ring = Ring::default();
        let mut pacer = ClockPacer::new(200, true);
        let now = Duration::ZERO;
        for level in [8000, 0, 8000] {
            ring.push(&pcm(level));
            let mut wait = pacer.offer(ring.len(), true, false, now);
            assert_eq!(pacer.tick(&mut wait, &mut ring, true, now), Some(pcm(level)));
            assert_eq!(pacer.conceals(false), level != 0);
        }
        assert!(pacer.offer(0, true, true, now).is_none());
        assert!(pacer.offer(0, false, false, now).is_none());
        let mut cancelled = pacer.offer(0, false, true, now);
        assert!(pacer.tick(&mut cancelled, &mut ring, false, now).is_none());
        let mut wait = pacer.offer(0, false, true, now);
        assert_eq!(wait.unwrap().due(), PERIOD * 2);
        assert_eq!(
            pacer.tick(&mut wait, &mut ring, false, PERIOD * 2),
            Some(concealment(&pcm(8000)))
        );
        assert!(!pacer.conceals(false));
        ring.push(&pcm(8000));
        let mut wait = pacer.offer(ring.len(), false, false, PERIOD * 2);
        let mut resumed = pcm(8000);
        fade_in(&mut resumed);
        assert_eq!(pacer.tick(&mut wait, &mut ring, false, PERIOD * 2), Some(resumed));
        let playout = pacer.take_playout();
        assert_eq!((playout.reprimes, playout.concealed, playout.fade_ins), (0, 1, 1));
        assert_eq!(pacer.take_playout(), Playout::default());
    }

    #[test]
    fn pacer_playout_stepped_sim_bypasses_prime_and_fade() {
        let mut pacer = ClockPacer::new(200, false);
        let mut ring = Ring::default();
        assert_eq!(pacer.retarget(PRIME * 3), Duration::ZERO);
        let frame = pcm(8000);
        ring.push(&frame);
        let mut wait = pacer.offer(ring.len(), false, true, Duration::ZERO);
        assert_eq!(pacer.tick(&mut wait, &mut ring, false, Duration::ZERO), Some(frame));
        assert!(!pacer.conceals(false));
        assert!(pacer.offer(0, false, true, Duration::ZERO).is_none());
        assert_eq!(pacer.take_playout(), Playout::default());
    }
}
