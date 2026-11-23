//! Fixed-size batches of mouth samples timed against the modeled speaker.
use echo_audio::{LipAnalyser, LipOptions, Vowel};
use echo_core_api::{PacerLipBatch, PacerLipSample, PacerVowel};
use std::time::Duration;

const BATCH_MAX: usize = 5;
const STALE_MS: u64 = 1000;

/// What the screen was sent to move the mouth with one reply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LipStats {
    pub batches: u32,
    pub samples: u32,
    pub dropped: u32,
    pub publish_max_ms: u32,
    pub lead_min_ms: u32,
    pub late: u32,
}

pub(crate) struct Lips {
    delay: Duration,
    epoch: Duration,
    size: usize,
    analyser: LipAnalyser,
    pending: [Option<PacerLipSample>; BATCH_MAX],
    len: usize,
    handed: Option<Duration>,
    wanted: bool,
    moving: bool,
    stats: LipStats,
}

fn analyser() -> LipAnalyser {
    LipAnalyser::new(24_000, LipOptions::default())
}

impl Lips {
    pub(crate) fn new(delay: Duration, epoch: Duration) -> Self {
        let size = usize::try_from(delay.as_millis() / 40).unwrap_or(BATCH_MAX);
        Self {
            delay,
            epoch,
            size: size.clamp(1, BATCH_MAX),
            analyser: analyser(),
            pending: [None; BATCH_MAX],
            len: 0,
            handed: None,
            wanted: false,
            moving: false,
            stats: LipStats::default(),
        }
    }

    fn clock(&self, at: Duration) -> u64 {
        u64::try_from(at.saturating_sub(self.epoch).as_millis()).unwrap_or(u64::MAX)
    }

    pub(crate) fn wrote(
        &mut self,
        frame: &[u8],
        start: Duration,
        now: Duration,
        wanted: Option<bool>,
    ) -> Option<PacerLipBatch> {
        self.wanted = wanted.unwrap_or(self.wanted);
        if !self.wanted {
            if !self.analyser.at_rest() {
                self.analyser.reset();
            }
            self.clear_pending();
            self.moving = false;
            return None;
        }
        let sample = self.analyser.window(frame);
        if self.pending[0].is_some_and(|first| first.at_ms.saturating_add(STALE_MS) <= self.clock(now)) {
            self.clear_pending();
        }
        if sample.open == 0 && !self.moving {
            return None;
        }
        self.moving = sample.open > 0;
        self.handed.get_or_insert(now);
        self.pending[self.len] = Some(PacerLipSample {
            at_ms: self.clock(start + self.delay),
            open: sample.open,
            vowel: match sample.vowel {
                None => PacerVowel::None,
                Some(Vowel::A) => PacerVowel::A,
                Some(Vowel::I) => PacerVowel::I,
                Some(Vowel::U) => PacerVowel::U,
                Some(Vowel::E) => PacerVowel::E,
                Some(Vowel::O) => PacerVowel::O,
            },
        });
        self.len += 1;
        if self.len >= self.size || !self.moving {
            self.flush(now)
        } else {
            None
        }
    }

    fn flush(&mut self, now: Duration) -> Option<PacerLipBatch> {
        let first = self.pending[0]?;
        let handed = self.handed.take()?;
        let sent_ms = self.clock(now);
        let lead = u32::try_from(first.at_ms.saturating_sub(sent_ms)).unwrap_or(u32::MAX);
        let stats = &mut self.stats;
        stats.lead_min_ms = if stats.batches == 0 {
            lead
        } else {
            stats.lead_min_ms.min(lead)
        };
        stats.late += u32::from(lead == 0);
        stats.batches += 1;
        stats.samples += u32::try_from(self.len).unwrap_or(u32::MAX);
        let waited = u32::try_from(now.saturating_sub(handed).as_millis()).unwrap_or(u32::MAX);
        stats.publish_max_ms = stats.publish_max_ms.max(waited);
        self.len = 0;
        Some(PacerLipBatch {
            sent_ms,
            samples: std::mem::take(&mut self.pending),
        })
    }

    fn clear_pending(&mut self) {
        self.pending = [None; BATCH_MAX];
        self.len = 0;
        self.handed = None;
    }

    pub(crate) fn delivered(&mut self, taken: bool) {
        self.stats.dropped += u32::from(!taken);
    }

    /// Shut the mouth and let the page drop its scheduled samples.
    pub(crate) fn cut(&mut self, now: Duration) -> Option<PacerLipBatch> {
        self.clear_pending();
        self.moving = false;
        self.analyser.reset();
        if !self.wanted {
            return None;
        }
        let sent_ms = self.clock(now);
        let shut = PacerLipSample {
            at_ms: sent_ms,
            open: 0,
            vowel: PacerVowel::None,
        };
        self.stats.batches += 1;
        self.stats.samples += 1;
        Some(PacerLipBatch {
            sent_ms,
            samples: [Some(shut), None, None, None, None],
        })
    }

    /// Pending samples still ahead of their sound leave now.
    pub(crate) fn settle(&mut self, now: Duration) -> Option<PacerLipBatch> {
        self.moving = false;
        let now_ms = self.clock(now);
        let pending = std::mem::take(&mut self.pending);
        self.len = 0;
        for sample in pending.into_iter().flatten().filter(|sample| sample.at_ms > now_ms) {
            self.pending[self.len] = Some(sample);
            self.len += 1;
        }
        if self.len == 0 {
            self.handed = None;
        }
        self.flush(now)
    }

    pub(crate) fn stats(&self) -> LipStats {
        self.stats
    }

    pub(crate) fn clear_stats(&mut self) {
        self.stats = LipStats::default();
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.len > 0
    }

    pub(crate) fn abandon(&mut self) {
        self.clear_pending();
        self.moving = false;
        self.clear_stats();
    }
}
