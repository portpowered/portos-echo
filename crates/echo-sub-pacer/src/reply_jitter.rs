//! How late reply frames have recently been, and the cushion that covers it.
use std::time::Duration;

const PERIOD_MS: i64 = 20;
/// The cushion a smooth call gets: gaps up to 62 ms between 20 ms frames
/// never ran a 100 ms cushion dry on the Echo (`docs/program/baselines.md`).
pub(crate) const FLOOR: Duration = Duration::from_millis(100);
/// The most reply audio held back for jitter. It covers the 270 to 280 ms
/// stalls measured on the Echo (any gap up to 300 ms) and bounds what a stall
/// can add to the next reply's first audio at `CEILING - FLOOR`.
const CEILING: Duration = Duration::from_millis(320);
/// Kept above the worst lateness: when a frame is due the Echo holds one
/// period less than the cushion, and the pacer fades out with one period left.
const MARGIN_MS: u32 = 40;
/// A stall counts in full this long. The stalls measured on the Echo came
/// 20 to 50 s apart, across replies (`docs/program/baselines.md`): a hold of
/// 20 s let the cushion decay between them, and four stalls in five ran the
/// Echo dry. The cushion stays up while they keep coming.
const HOLD: Duration = Duration::from_secs(120);
/// After that it is forgotten at 5 ms a second.
const DECAY_MS_PER_SECOND: u128 = 5;
/// A silence this long is the model pausing, not the network: start afresh.
const RESYNC: Duration = Duration::from_secs(1);
/// The lateness a call is taken to have before it has shown any: every call
/// measured on the Echo stalled 146 to 299 ms in its first reply
/// (`docs/program/baselines.md`), and a first reply that started on the floor
/// ran the Echo dry there. It asks for 300 ms and is forgotten like a stall.
pub(crate) const EXPECTED_MS: u32 = 260;
/// Frames in one half of the sliding minimum the lateness is measured from.
const WINDOW: u32 = 50;

/// Lateness of each accepted frame against the 20 ms audio clock, measured
/// from the earliest frame of the last one to two seconds, so lost frames or
/// a path change shift the reference instead of counting as lateness for
/// ever. The worst lateness is held, then decays with time; the playout
/// target is that lateness plus the margin, in whole frames, within the bounds.
#[derive(Default)]
pub(crate) struct Jitter {
    stream: Option<Stream>,
    peak: Option<(u32, Duration)>,
    /// Lateness to assume from the call's first frame.
    expected: Option<u32>,
}

struct Stream {
    anchor: Duration,
    last: Duration,
    frames: u32,
    /// Smallest offsets from the audio clock: previous window, this window.
    early: [i64; 2],
    filled: u32,
}

impl Jitter {
    /// A call that is taken to stall as calls on the Echo have.
    pub(crate) fn expecting(late_ms: u32) -> Self {
        Self {
            expected: Some(late_ms),
            ..Self::default()
        }
    }

    /// One reply frame was accepted at `now`.
    pub(crate) fn arrived(&mut self, now: Duration) {
        if let Some(expected) = self.expected.take() {
            self.peak = Some((expected, now));
        }
        let stream = match &mut self.stream {
            Some(stream) if now.saturating_sub(stream.last) <= RESYNC => stream,
            stream => stream.insert(Stream {
                anchor: now,
                last: now,
                frames: 0,
                early: [i64::MAX; 2],
                filled: 0,
            }),
        };
        let late = stream.late(now);
        if late >= self.held(now) {
            self.peak = Some((late, now));
        }
    }

    /// Frames between here and the next arrival are not part of the reply's
    /// stream: its clock starts again, and what was learned is kept.
    pub(crate) fn rest(&mut self) {
        self.stream = None;
    }

    /// The worst recent lateness as it counts at `now`.
    fn held(&self, now: Duration) -> u32 {
        let Some((peak, at)) = self.peak else {
            return 0;
        };
        let fading = now.saturating_sub(at).saturating_sub(HOLD);
        let forgotten = fading.as_millis() * DECAY_MS_PER_SECOND / 1000;
        peak.saturating_sub(u32::try_from(forgotten).unwrap_or(u32::MAX))
    }

    /// The reply audio to hold ahead of playout at `now`.
    pub(crate) fn target(&self, now: Duration) -> Duration {
        let wanted = self.held(now).saturating_add(MARGIN_MS).div_ceil(20) * 20;
        Duration::from_millis(u64::from(wanted)).clamp(FLOOR, CEILING)
    }
}

impl Stream {
    fn late(&mut self, now: Duration) -> u32 {
        let elapsed = i64::try_from(now.saturating_sub(self.anchor).as_millis()).unwrap_or(i64::MAX);
        let offset = elapsed.saturating_sub(PERIOD_MS * i64::from(self.frames));
        self.last = now;
        self.frames = self.frames.saturating_add(1);
        if self.filled == WINDOW {
            self.early = [self.early[1], i64::MAX];
            self.filled = 0;
        }
        self.filled += 1;
        self.early[1] = self.early[1].min(offset);
        let earliest = self.early[0].min(self.early[1]);
        u32::try_from(offset.saturating_sub(earliest)).unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(jitter: &mut Jitter, now: &mut Duration, gaps_ms: &[u64]) {
        for gap in gaps_ms {
            *now += Duration::from_millis(*gap);
            jitter.arrived(*now);
        }
    }

    fn target_ms(jitter: &Jitter, now: Duration) -> u128 {
        jitter.target(now).as_millis()
    }

    #[test]
    fn reply_jitter_steady_and_measured_arrivals_keep_the_floor() {
        let (mut jitter, mut now) = (Jitter::default(), Duration::ZERO);
        assert_eq!(target_ms(&jitter, now), 100);
        feed(&mut jitter, &mut now, &[20; 500]);
        assert_eq!((jitter.held(now), target_ms(&jitter, now)), (0, 100));
        // The gaps measured on the Echo: the worst frame is 42 ms late.
        let measured = [
            62, 2, 4, 12, 36, 4, 20, 20, 38, 2, 20, 20, 45, 3, 12, 20, 28, 12, 20, 20,
        ];
        for _ in 0..50 {
            feed(&mut jitter, &mut now, &measured);
        }
        assert_eq!((jitter.held(now), target_ms(&jitter, now)), (42, 100));
    }

    #[test]
    fn reply_jitter_stall_raises_the_target_which_decays_to_the_floor() {
        let (mut jitter, mut now) = (Jitter::default(), Duration::ZERO);
        feed(&mut jitter, &mut now, &[20; 100]);
        // A 270 ms stall, then the 13 frames it delayed arrive together.
        feed(&mut jitter, &mut now, &[270]);
        assert_eq!((jitter.held(now), target_ms(&jitter, now)), (250, 300));
        feed(&mut jitter, &mut now, &[0; 12]);
        feed(&mut jitter, &mut now, &[10, 20, 20]);
        assert_eq!((jitter.held(now), target_ms(&jitter, now)), (250, 300));
        // Held in full for 120 s, then 5 ms a second: 200 ms at 130 s, then 0.
        feed(&mut jitter, &mut now, &vec![20; 5998]);
        assert_eq!((jitter.held(now), target_ms(&jitter, now)), (250, 300));
        feed(&mut jitter, &mut now, &[20; 500]);
        assert_eq!((jitter.held(now), target_ms(&jitter, now)), (200, 240));
        feed(&mut jitter, &mut now, &[20; 2000]);
        assert_eq!((jitter.held(now), target_ms(&jitter, now)), (0, 100));
        // A stall longer than the ceiling covers is bounded by it.
        feed(&mut jitter, &mut now, &[443]);
        assert_eq!((jitter.held(now), target_ms(&jitter, now)), (423, 320));
    }

    #[test]
    fn reply_jitter_lost_frames_and_pauses_are_not_lateness_for_ever() {
        let (mut jitter, mut now) = (Jitter::default(), Duration::ZERO);
        feed(&mut jitter, &mut now, &[20; 100]);
        // 200 ms of frames never come: every later frame is 180 ms "late"
        // until the sliding minimum has moved past the loss.
        feed(&mut jitter, &mut now, &[200]);
        assert_eq!(jitter.held(now), 180);
        feed(&mut jitter, &mut now, &[20; 100]);
        let moved = now;
        feed(&mut jitter, &mut now, &[20; 50]);
        assert_eq!(jitter.peak.map(|(late, at)| (late, at < moved)), Some((180, true)));
        // The model pausing for 3 s, and the gap between replies, are not jitter.
        feed(&mut jitter, &mut now, &[3000, 20, 20]);
        jitter.rest();
        feed(&mut jitter, &mut now, &[500, 20, 20]);
        assert_eq!(jitter.peak.map(|(late, at)| (late, at < moved)), Some((180, true)));
        let mut fresh = Jitter::default();
        feed(&mut fresh, &mut now, &[20, 3000, 20, 500]);
        assert_eq!(fresh.held(now), 480);
    }
}
