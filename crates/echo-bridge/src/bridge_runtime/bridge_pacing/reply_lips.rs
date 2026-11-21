//! The companion's lips, from the reply audio handed to the Echo. Each 20 ms
//! frame becomes one mouth sample stamped with the instant its sound leaves
//! the speaker; samples go to the screen ahead of that instant in small
//! batches. Nothing here waits: a screen that is slow, busy or absent loses
//! batches and the audio path never learns of it.
use echo_audio::{LipAnalyser, LipOptions, Vowel};
use echo_screen_contract::mouth::{MouthBatch, MouthSample, MouthVowel};
use echo_screen_server::MouthSender;
use std::{
    future::Future,
    sync::{Mutex, MutexGuard, PoisonError},
};
use tokio::time::{Duration, Instant};

/// Reply audio is 24 kHz s16le mono.
const RATE: u16 = 24_000;
/// A batch never holds more than 100 ms of samples.
const BATCH_MAX: usize = 5;

/// Where batches go. Neither call may wait.
pub(crate) trait LipSink: Send {
    /// Some screen asked for lip sync; none when that is unknown this instant.
    fn wanted(&self) -> Option<bool>;
    /// How many screens took the batch.
    fn send(&self, batch: &MouthBatch) -> usize;
}

impl LipSink for MouthSender {
    fn wanted(&self) -> Option<bool> {
        MouthSender::wanted(self)
    }

    fn send(&self, batch: &MouthBatch) -> usize {
        MouthSender::send(self, batch)
    }
}

/// One reply's lip-sync facts for `reply.quality`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LipStats {
    /// Batches offered to the screen, and the samples in them.
    pub batches: u32,
    pub samples: u32,
    /// Batches no screen took: it was behind, or the server was busy.
    pub dropped: u32,
    /// Longest wait from a frame's hand-off to the Echo until its sample left.
    pub publish_max_ms: u32,
    /// Shortest time a batch left ahead of its first sample's playback.
    pub lead_min_ms: u32,
    /// Batches that left after their first sample was due.
    pub late: u32,
}

pub(crate) struct Lips {
    sink: Box<dyn LipSink>,
    delay: Duration,
    epoch: Instant,
    size: usize,
    analyser: LipAnalyser,
    pending: Vec<MouthSample>,
    /// Hand-off of the oldest pending sample's frame.
    handed: Option<Instant>,
    wanted: bool,
    /// The last sample sent or pending shows an open mouth.
    moving: bool,
    stats: LipStats,
}

fn millis(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

fn analyser() -> LipAnalyser {
    LipAnalyser::new(RATE, LipOptions::default())
}

impl Lips {
    /// `delay` is from a frame's hand-off to the Echo until the speaker
    /// plays it. A batch must leave before its first sample is due, so it
    /// holds at most half the delay.
    pub(crate) fn new(sink: Box<dyn LipSink>, delay: Duration) -> Self {
        let size = usize::try_from(delay.as_millis() / 40).unwrap_or(BATCH_MAX);
        Self {
            sink,
            delay,
            epoch: Instant::now(),
            size: size.clamp(1, BATCH_MAX),
            analyser: analyser(),
            pending: Vec::with_capacity(BATCH_MAX),
            handed: None,
            wanted: false,
            moving: false,
            stats: LipStats::default(),
        }
    }

    fn clock(&self, at: Instant) -> u64 {
        u64::try_from(at.saturating_duration_since(self.epoch).as_millis()).unwrap_or(u64::MAX)
    }

    /// One reply frame was handed to the Echo at `now` and starts playing
    /// out of its queue at `start`.
    fn wrote(&mut self, frame: &[u8], start: Instant, now: Instant) {
        self.wanted = self.sink.wanted().unwrap_or(self.wanted);
        if !self.wanted {
            // Nobody draws a mouth: spend nothing on analysis.
            if !self.analyser.at_rest() {
                self.analyser = analyser();
            }
            self.pending.clear();
            self.handed = None;
            self.moving = false;
            return;
        }
        let sample = self.analyser.window(frame);
        if sample.open == 0 && !self.moving {
            return;
        }
        self.moving = sample.open > 0;
        self.handed.get_or_insert(now);
        self.pending.push(MouthSample {
            at_ms: self.clock(start + self.delay),
            open: sample.open,
            vowel: match sample.vowel {
                None => MouthVowel::None,
                Some(Vowel::A) => MouthVowel::A,
                Some(Vowel::I) => MouthVowel::I,
                Some(Vowel::U) => MouthVowel::U,
                Some(Vowel::E) => MouthVowel::E,
                Some(Vowel::O) => MouthVowel::O,
            },
        });
        // The sample that shuts the mouth leaves at once.
        if self.pending.len() >= self.size || !self.moving {
            self.flush(now);
        }
    }

    fn flush(&mut self, now: Instant) {
        let (Some(first), Some(handed)) = (self.pending.first(), self.handed.take()) else {
            return;
        };
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
        stats.samples += u32::try_from(self.pending.len()).unwrap_or(u32::MAX);
        stats.publish_max_ms = stats.publish_max_ms.max(millis(now.saturating_duration_since(handed)));
        let batch = MouthBatch {
            sent_ms,
            samples: std::mem::replace(&mut self.pending, Vec::with_capacity(BATCH_MAX)),
        };
        if self.sink.send(&batch) == 0 {
            stats.dropped += 1;
        }
    }

    /// Close this reply's window. Pending samples still ahead of their sound
    /// leave now; ones already heard are dropped, and the page shuts a mouth
    /// left open when no sample has come for a moment.
    fn settle(&mut self, now: Instant) -> LipStats {
        self.moving = false;
        let now_ms = self.clock(now);
        self.pending.retain(|sample| sample.at_ms > now_ms);
        if self.pending.is_empty() {
            self.handed = None;
        }
        self.flush(now);
        std::mem::take(&mut self.stats)
    }
}

tokio::task_local! { static CURRENT: Mutex<Lips>; }

fn lock(lips: &Mutex<Lips>) -> MutexGuard<'_, Lips> {
    lips.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Run `work` with these lips following the reply audio it hands to the Echo.
pub(crate) async fn scope<F: Future>(lips: Option<Lips>, work: F) -> F::Output {
    match lips {
        Some(lips) => CURRENT.scope(Mutex::new(lips), work).await,
        None => work.await,
    }
}

/// One reply frame was handed to the Echo at `now`; the modelled device
/// queue starts playing it at `start`. Outside a scope this does nothing.
pub(crate) fn wrote(frame: &[u8], start: Instant, now: Instant) {
    let _ = CURRENT.try_with(|lips| lock(lips).wrote(frame, start, now));
}

/// Close the reply's lip-sync window.
pub(crate) fn settle(now: Instant) -> LipStats {
    CURRENT.try_with(|lips| lock(lips).settle(now)).unwrap_or_default()
}

#[cfg(test)]
#[path = "reply_lips_tests.rs"]
mod tests;
