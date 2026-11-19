use echo_audio::{FrameQueue, Pacer};
use std::time::Duration;
use tokio::time::Instant;

/// Initial playback credit followed by the existing no-catch-up 20 ms pacer.
pub(crate) struct LeadPacer {
    remaining: u16,
    started: Option<Instant>,
    pacer: Pacer,
}

impl LeadPacer {
    pub(crate) fn new(lead_ms: u16) -> Self {
        Self {
            remaining: lead_ms / 20,
            started: None,
            pacer: Pacer::new(),
        }
    }

    pub(crate) async fn next_frame<T>(&mut self, queue: &mut FrameQueue<T>) -> Option<T> {
        if queue.is_empty() {
            return None;
        }
        let started = *self.started.get_or_insert_with(|| {
            self.pacer = Pacer::new();
            Instant::now()
        });
        // Underflow never renews credit, and a late producer never catches up.
        if Instant::now() >= started + Duration::from_millis(20) {
            self.remaining = 0;
        }
        if self.remaining > 0 {
            self.remaining -= 1;
            queue.pop()
        } else {
            self.pacer.next_frame(queue).await
        }
    }
}

const PERIOD: Duration = Duration::from_millis(20);

/// The cushion a dry Echo gets before playback starts. Frames from the call
/// arrive in real time with jitter (measured on the Echo: gaps up to 62 ms
/// between 20 ms frames, `docs/program/baselines.md`); forwarded one by one
/// into an empty player, every late frame is an audible underrun.
const PRIME: Duration = Duration::from_millis(100);

/// Audio-clock pacing for a reply that arrives in real time (the WebRTC peer).
/// A dry device first gets `PRIME` of audio (at most the lead) at once: the
/// pacer waits for that much to gather, for the end of the reply, or `PRIME`
/// from when the frame was first offered, whichever is first, so a burst is
/// never delayed and no frame is held longer than `PRIME`.
/// After that frames go out as they arrive, never more than `lead` ahead of
/// the clock: frame k is due at `start + 20 ms * (k + 1) - lead`. Late
/// wakeups (a coarse OS timer) catch up instead of stretching every frame. Once
/// the device has played out everything sent, the clock restarts the same way.
pub(crate) struct ClockPacer {
    lead_frames: u32,
    prime: Duration,
    /// When the frame at the head of the queue was first offered.
    offered: Option<Instant>,
    started: Option<Instant>,
    sent: u32,
}

impl ClockPacer {
    pub(crate) fn new(lead_ms: u16) -> Self {
        let lead_frames = u32::from(lead_ms / 20);
        // The sim's stepped clock stands still while a released reply plays
        // on the device's wall clock: a timed hold would never end there.
        let stepped = crate::bridge_mcp::bridge_clock::receipt::Context::current().is_some();
        Self {
            lead_frames,
            prime: PRIME.min(PERIOD * if stepped { 0 } else { lead_frames }),
            offered: None,
            started: None,
            sent: 0,
        }
    }

    /// Cancellation while waiting consumes no frame and no clock credit.
    /// `flush`: no more frames will follow, so nothing is worth waiting for.
    pub(crate) async fn next_frame<T>(&mut self, queue: &mut FrameQueue<T>, flush: bool) -> Option<T> {
        if queue.is_empty() {
            return None;
        }
        let mut now = Instant::now();
        // The hold survives cancelled polls: it runs from the first offer.
        let release = *self.offered.get_or_insert(now) + self.prime;
        let started = match self.started {
            Some(started) if started + PERIOD * self.sent >= now => started,
            _ => {
                let primed = flush || PERIOD * u32::try_from(queue.len()).unwrap_or(u32::MAX) >= self.prime;
                if !primed && release > now {
                    tokio::time::sleep_until(release).await;
                    now = Instant::now();
                }
                self.sent = 0;
                *self.started.insert(now)
            }
        };
        let due = started + (PERIOD * (self.sent + 1)).saturating_sub(PERIOD * self.lead_frames);
        // Timers round up to the next millisecond: never sleep for a frame already due.
        if due > now {
            tokio::time::sleep_until(due).await;
        }
        self.sent += 1;
        self.offered = None;
        queue.pop()
    }
}

/// One reply's audio facts for `reply.played`. Depth is the Echo's modelled
/// playout queue: frames written play back to back from the first write.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReplyQuality {
    /// Longest wait between two accepted frames from the call.
    pub arrival_gap_max_ms: u32,
    /// Accepted frames that came more than two periods after the previous one.
    pub late_arrivals: u32,
    /// Writes that found the modelled queue empty after playback had started.
    pub underruns: u32,
    pub underrun_max_ms: u32,
    /// Queue depth found by each write after the first.
    pub depth_min_ms: u32,
    pub depth_max_ms: u32,
    pub discarded_after_reply: u32,
    pub discarded_idle: u32,
    pub discarded_cut: u32,
    /// Discarded frames, for any reason, that held more than silence.
    pub discarded_audible: u32,
    /// RTP packets missing from the receive sequence during this reply.
    pub rtp_lost: u64,
}

/// Why accepted call audio never reached the reply queue.
#[derive(Clone, Copy)]
pub(crate) enum Discard {
    AfterReply,
    Idle,
    Cut,
}

#[derive(Default)]
pub(crate) struct ReplyMeter {
    quality: ReplyQuality,
    arrived: Option<Instant>,
    playout: Option<Instant>,
    measured: bool,
    lost: u64,
}

fn millis(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

impl ReplyMeter {
    pub(crate) fn arrived(&mut self, now: Instant) {
        if let Some(last) = self.arrived.replace(now) {
            let gap = now.saturating_duration_since(last);
            self.quality.arrival_gap_max_ms = self.quality.arrival_gap_max_ms.max(millis(gap));
            if gap > PERIOD * 2 {
                self.quality.late_arrivals += 1;
            }
        }
    }

    /// One 20 ms frame was handed to the Echo at `now`.
    pub(crate) fn wrote(&mut self, now: Instant) {
        let Some(end) = self.playout else {
            self.playout = Some(now + PERIOD);
            return;
        };
        let depth = millis(end.saturating_duration_since(now));
        if depth == 0 {
            self.quality.underruns += 1;
            self.quality.underrun_max_ms = self
                .quality
                .underrun_max_ms
                .max(millis(now.saturating_duration_since(end)));
        }
        self.quality.depth_min_ms = if self.measured {
            self.quality.depth_min_ms.min(depth)
        } else {
            depth
        };
        self.quality.depth_max_ms = self.quality.depth_max_ms.max(depth);
        self.measured = true;
        self.playout = Some(end.max(now) + PERIOD);
    }

    pub(crate) fn discarded(&mut self, reason: Discard, audible: bool) {
        let count = match reason {
            Discard::AfterReply => &mut self.quality.discarded_after_reply,
            Discard::Idle => &mut self.quality.discarded_idle,
            Discard::Cut => &mut self.quality.discarded_cut,
        };
        *count += 1;
        self.quality.discarded_audible += u32::from(audible);
    }

    /// Close this reply's window. `lost` is the call's cumulative RTP loss.
    pub(crate) fn settle(&mut self, lost: u64) -> ReplyQuality {
        let mut quality = std::mem::take(&mut self.quality);
        quality.rtp_lost = lost.saturating_sub(self.lost);
        *self = Self {
            lost,
            ..Self::default()
        };
        quality
    }
}

#[cfg(test)]
mod bridge_pacing_tests {
    use super::*;
    use std::{future::Future, num::NonZeroUsize, task::Poll};

    fn queue() -> FrameQueue<u16> {
        let mut queue = FrameQueue::new(NonZeroUsize::new(30).unwrap());
        for frame in 0..30 {
            queue.push(frame);
        }
        queue
    }

    /// Arrival gaps in ms measured on the Echo's armv7 bridge, worst first:
    /// the longest was 62 ms, 7 of 2064 exceeded 40 ms, 108 fell in 26..=40.
    const JITTER: [u64; 20] = [
        62, 2, 4, 12, 36, 4, 20, 20, 38, 2, 20, 20, 45, 3, 12, 20, 28, 12, 20, 20,
    ];

    /// Forward a jittered real-time stream and report the Echo's modelled queue.
    async fn jittered(mut pacer: ClockPacer, frames: usize) -> ReplyQuality {
        let mut queue = FrameQueue::new(NonZeroUsize::new(128).unwrap());
        let mut meter = ReplyMeter::default();
        let mut arrival = Instant::now();
        for frame in 0..frames {
            arrival += Duration::from_millis(JITTER[frame % JITTER.len()]);
            // Forward everything due before the next frame arrives.
            while !queue.is_empty() {
                tokio::select! {
                    biased;
                    sent = pacer.next_frame(&mut queue, false) => { sent.unwrap(); meter.wrote(Instant::now()); }
                    () = tokio::time::sleep_until(arrival) => break,
                }
            }
            tokio::time::sleep_until(arrival).await;
            meter.arrived(arrival);
            queue.push(frame);
        }
        while pacer.next_frame(&mut queue, true).await.is_some() {
            meter.wrote(Instant::now());
        }
        meter.settle(0)
    }

    #[tokio::test(start_paused = true)]
    async fn bridge_pacing_measured_jitter_never_runs_the_primed_echo_dry() {
        assert_eq!(JITTER.iter().sum::<u64>(), 20 * 20);
        let quality = jittered(ClockPacer::new(200), 1000).await;
        assert_eq!((quality.arrival_gap_max_ms, quality.late_arrivals), (62, 99));
        assert_eq!((quality.underruns, quality.underrun_max_ms), (0, 0));
        assert!(quality.depth_min_ms >= 20, "{quality:?}");
        assert!(quality.depth_max_ms <= 200, "{quality:?}");
        // The regression this guards: the same stream forwarded frame by frame.
        let unprimed = ClockPacer {
            prime: Duration::ZERO,
            ..ClockPacer::new(200)
        };
        let quality = jittered(unprimed, 1000).await;
        assert!(quality.underruns >= 40, "{quality:?}");
        assert_eq!(quality.depth_min_ms, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn bridge_pacing_prime_is_bounded_by_lead_and_survives_cancelled_polls() {
        let mut queue = FrameQueue::new(NonZeroUsize::new(30).unwrap());
        queue.push(0);
        queue.push(1);
        let mut pacer = ClockPacer::new(60);
        let start = Instant::now();
        for _ in 0..3 {
            let mut next = Box::pin(pacer.next_frame(&mut queue, false));
            std::future::poll_fn(|cx| {
                assert!(next.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            drop(next);
            tokio::time::advance(Duration::from_millis(10)).await;
        }
        assert_eq!(queue.len(), 2);
        // Two frames are under the 60 ms prime: held 60 ms from the first poll.
        assert_eq!(pacer.next_frame(&mut queue, false).await, Some(0));
        assert_eq!(Instant::now() - start, Duration::from_millis(60));
        assert_eq!(pacer.next_frame(&mut queue, false).await, Some(1));
        assert_eq!(Instant::now() - start, Duration::from_millis(60));
        // A dry device again: the end of the reply and a full prime never wait.
        tokio::time::advance(Duration::from_secs(1)).await;
        let late = Instant::now();
        queue.push(2);
        assert_eq!(pacer.next_frame(&mut queue, true).await, Some(2));
        tokio::time::advance(Duration::from_secs(1)).await;
        for frame in 3..6 {
            queue.push(frame);
        }
        assert_eq!(pacer.next_frame(&mut queue, false).await, Some(3));
        assert_eq!(Instant::now() - late, Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn bridge_pacing_stepped_sim_clock_never_holds_a_frame() {
        // The M4 sim releases five frames, then waits for them to play
        // before it steps the bridge clock again.
        let clock = crate::bridge_mcp::bridge_clock::receipt::Context::for_voice_test();
        let mut pacer = clock.owning(async { ClockPacer::new(200) }).await;
        let mut queue = FrameQueue::new(NonZeroUsize::new(4).unwrap());
        queue.push(0);
        let start = Instant::now();
        assert_eq!(pacer.next_frame(&mut queue, false).await, Some(0));
        assert_eq!(Instant::now(), start);
    }

    #[test]
    fn reply_meter_counts_discards_by_reason_and_settles_per_reply() {
        let mut meter = ReplyMeter::default();
        meter.discarded(Discard::AfterReply, true);
        meter.discarded(Discard::Idle, false);
        meter.discarded(Discard::Cut, true);
        meter.discarded(Discard::Cut, false);
        let first = meter.settle(5);
        assert_eq!(
            (first.discarded_after_reply, first.discarded_idle, first.discarded_cut),
            (1, 1, 2)
        );
        assert_eq!((first.discarded_audible, first.rtp_lost), (2, 5));
        let second = meter.settle(7);
        assert_eq!(
            second,
            ReplyQuality {
                rtp_lost: 2,
                ..ReplyQuality::default()
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn bridge_pacing_ten_initial_frames_then_twenty_ms_without_catchup() {
        let mut queue = queue();
        let mut pacer = LeadPacer::new(200);
        let start = Instant::now();
        for frame in 0..10 {
            assert_eq!(pacer.next_frame(&mut queue).await, Some(frame));
        }
        assert_eq!(Instant::now(), start);
        assert_eq!(pacer.next_frame(&mut queue).await, Some(10));
        assert_eq!(Instant::now() - start, Duration::from_millis(20));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(pacer.next_frame(&mut queue).await, Some(11));
        let late = Instant::now();
        assert_eq!(pacer.next_frame(&mut queue).await, Some(12));
        assert_eq!(Instant::now() - late, Duration::from_millis(20));
    }

    #[tokio::test(start_paused = true)]
    async fn bridge_pacing_underflow_and_cancel_never_add_credit_or_consume_frames() {
        let mut queue = queue();
        let mut pacer = LeadPacer::new(20);
        assert_eq!(pacer.next_frame(&mut queue).await, Some(0));
        let mut next = Box::pin(pacer.next_frame(&mut queue));
        std::future::poll_fn(|cx| {
            assert!(next.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(next);
        assert_eq!(queue.len(), 29);
        queue.clear();
        assert_eq!(pacer.next_frame(&mut queue).await, None);
        tokio::time::advance(Duration::from_secs(1)).await;
        queue.push(40);
        queue.push(41);
        let now = Instant::now();
        assert_eq!(pacer.next_frame(&mut queue).await, Some(40));
        assert_eq!(pacer.next_frame(&mut queue).await, Some(41));
        assert_eq!(Instant::now() - now, Duration::from_millis(20));
    }
}
