//! The screen's mouth, timed from the audio written to the Echo.
use crate::bridge_runtime::bridge_pacing::{QUIET, reply_lips};
use std::collections::VecDeque;
use tokio::time::{Duration, Instant};

pub(super) const FRAME: Duration = Duration::from_millis(20);
/// Measured on the real Echo against a recording of its sound: a frame is
/// heard about 85 ms after its place on the modelled playout clock below.
/// (hassmic reports `played` later than that: it adds a fixed 120 ms tail.)
pub(super) const OUTPUT_DELAY: Duration = Duration::from_millis(80);
/// A quiet stretch inside a reply shorter than this is a breath between
/// words, not the end of speech: the mouth stays open through it. On the
/// Echo the state flipped twice within 20 to 250 ms at such gaps.
pub(super) const GAP_HOLD: Duration = Duration::from_millis(300);

/// The screen's mouth follows audio the Echo is actually playing, not model
/// events. Frames written to the Echo play back to back on its clock, which
/// restarts when the device ran dry. The mouth opens when the first audible
/// frame reaches the speaker and closes when the last one has played: at
/// once when the reply has no more audio to come, and otherwise only after
/// the quiet has lasted `GAP_HOLD`.
#[derive(Default)]
pub struct Mouth {
    clock: Option<Instant>,
    transitions: VecDeque<(Instant, bool)>,
    open: bool,
    /// The reply is complete: its last close is not held for more audio.
    settled: bool,
}

impl Mouth {
    /// Record one 24 kHz s16le frame written to the Echo at `now`.
    pub fn wrote(&mut self, frame: &[u8], now: Instant) {
        if frame.is_empty() {
            return;
        }
        self.settled = false;
        let start = self.clock.filter(|end| *end > now).unwrap_or(now);
        self.clock = Some(start + FRAME);
        reply_lips::wrote(frame, start, now);
        let audible = frame
            .as_chunks::<2>()
            .0
            .iter()
            .any(|sample| i16::from_le_bytes(*sample).unsigned_abs() > QUIET);
        if audible {
            let open = start + OUTPUT_DELAY;
            let close = open + FRAME;
            if let Some((due, false)) = self.transitions.back_mut()
                && open.saturating_duration_since(*due) < GAP_HOLD
            {
                *due = close;
            } else {
                self.transitions.push_back((open, true));
                self.transitions.push_back((close, false));
            }
        }
    }

    /// Every frame of the reply has been written: nothing can follow its
    /// last sound, so the mouth closes with it.
    pub fn settle(&mut self) {
        self.settled = true;
    }

    /// When the next open or close is due. The last close of a reply that
    /// may still bring audio waits out the gap hold.
    #[must_use]
    pub fn due(&self) -> Option<Instant> {
        let (due, open) = *self.transitions.front()?;
        let held = !open && !self.settled && self.transitions.len() == 1;
        Some(if held { due + GAP_HOLD } else { due })
    }

    /// The change due by `now`, if any: true opens the mouth, false closes it.
    pub fn poll(&mut self, now: Instant) -> Option<bool> {
        self.due().filter(|due| *due <= now)?;
        let (_, open) = self.transitions.pop_front()?;
        self.open = open;
        Some(open)
    }

    /// Audible reply audio is written to the Echo and not yet played out.
    #[must_use]
    pub fn voiced(&self) -> bool {
        self.open || self.due().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audible() -> Vec<u8> {
        [2000_i16.to_le_bytes(); 480].concat()
    }

    /// Every change the mouth makes over the next five seconds, in ms from `start`.
    async fn changes(mouth: &mut Mouth, start: Instant) -> Vec<(u128, bool)> {
        let mut seen = Vec::new();
        for _ in 0..500 {
            while let Some(open) = mouth.poll(Instant::now()) {
                seen.push(((Instant::now() - start).as_millis(), open));
            }
            tokio::time::advance(Duration::from_millis(10)).await;
        }
        seen
    }

    /// Write `frames` back to back from now, as the reply's lead allows.
    fn speak(mouth: &mut Mouth, frames: usize) {
        for _ in 0..frames {
            mouth.wrote(&audible(), Instant::now());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn mouth_stays_open_through_a_gap_shorter_than_the_hold() {
        let start = Instant::now();
        let mut mouth = Mouth::default();
        // 100 ms of speech, 280 ms of quiet, 100 ms of speech: one utterance.
        speak(&mut mouth, 5);
        for _ in 0..14 {
            mouth.wrote(&[0; 960], Instant::now());
        }
        speak(&mut mouth, 5);
        assert!(mouth.voiced());
        mouth.settle();
        assert_eq!(changes(&mut mouth, start).await, [(80, true), (560, false)]);
        assert!(!mouth.voiced());
    }

    #[tokio::test(start_paused = true)]
    async fn mouth_holds_its_close_for_audio_that_may_follow_and_not_after_the_reply_ends() {
        // The reply has more to come: the close waits out the hold, and audio
        // inside it keeps the mouth open.
        let start = Instant::now();
        let mut mouth = Mouth::default();
        speak(&mut mouth, 5);
        tokio::time::advance(Duration::from_millis(300)).await;
        assert_eq!(mouth.poll(Instant::now()), Some(true));
        // The speech ended at 180 ms. 120 ms later nothing has closed.
        assert_eq!(mouth.poll(Instant::now()), None);
        speak(&mut mouth, 5);
        mouth.settle();
        assert_eq!(changes(&mut mouth, start).await, [(480, false)]);
        // A quiet of the hold or longer does close the mouth, the hold late.
        let start = Instant::now();
        let mut mouth = Mouth::default();
        speak(&mut mouth, 5);
        assert_eq!(
            changes(&mut mouth, start).await,
            [(80, true), (180 + GAP_HOLD.as_millis(), false)]
        );
        // A finished reply closes with its last sound.
        let start = Instant::now();
        let mut mouth = Mouth::default();
        speak(&mut mouth, 5);
        mouth.settle();
        assert_eq!(changes(&mut mouth, start).await, [(80, true), (180, false)]);
    }
}
