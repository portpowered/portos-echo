//! The screen's mouth, timed from the audio written to the Echo.
use echo_core_api::REPLY_QUIET as QUIET;
use std::collections::VecDeque;
use std::time::Duration;

const FRAME: Duration = Duration::from_millis(20);
/// Measured on the real Echo against a recording of its sound: a frame is
/// heard about 85 ms after its place on the modelled playout clock below.
/// (hassmic reports `played` later than that: it adds a fixed 120 ms tail.)
const OUTPUT_DELAY: Duration = Duration::from_millis(80);
/// A quiet stretch inside a reply shorter than this is a breath between
/// words, not the end of speech: the mouth stays open through it. On the
/// Echo the state flipped twice within 20 to 250 ms at such gaps.
const GAP_HOLD: Duration = Duration::from_millis(300);

/// The screen's mouth follows audio the Echo is actually playing, not model
/// events. Frames written to the Echo play back to back on its clock, which
/// restarts when the device ran dry. The mouth opens when the first audible
/// frame reaches the speaker and closes when the last one has played: at
/// once when the reply has no more audio to come, and otherwise only after
/// the quiet has lasted `GAP_HOLD`.
#[derive(Default)]
pub(crate) struct Mouth {
    clock: Option<Duration>,
    transitions: VecDeque<(Duration, bool)>,
    open: bool,
    /// The reply is complete: its last close is not held for more audio.
    settled: bool,
}

impl Mouth {
    /// Record one 24 kHz s16le frame written to the Echo at `now`.
    pub(crate) fn wrote(&mut self, frame: &[u8], now: Duration) -> Duration {
        if frame.is_empty() {
            return now;
        }
        self.settled = false;
        let start = self.clock.filter(|end| *end > now).unwrap_or(now);
        self.clock = Some(start + FRAME);
        let audible = frame
            .as_chunks::<2>()
            .0
            .iter()
            .any(|sample| i16::from_le_bytes(*sample).unsigned_abs() > QUIET);
        if audible {
            let open = start + OUTPUT_DELAY;
            let close = open + FRAME;
            if let Some((due, false)) = self.transitions.back_mut()
                && open.saturating_sub(*due) < GAP_HOLD
            {
                *due = close;
            } else {
                self.transitions.push_back((open, true));
                self.transitions.push_back((close, false));
            }
        }
        start
    }

    /// Every frame of the reply has been written: nothing can follow its
    /// last sound, so the mouth closes with it.
    pub(crate) fn settle(&mut self) {
        self.settled = true;
    }

    /// When the next open or close is due. The last close of a reply that
    /// may still bring audio waits out the gap hold.
    #[must_use]
    pub(crate) fn due(&self) -> Option<Duration> {
        let (due, open) = *self.transitions.front()?;
        let held = !open && !self.settled && self.transitions.len() == 1;
        Some(if held { due + GAP_HOLD } else { due })
    }

    /// The change due by `now`, if any: true opens the mouth, false closes it.
    pub(crate) fn poll(&mut self, now: Duration) -> Option<bool> {
        self.due().filter(|due| *due <= now)?;
        let (_, open) = self.transitions.pop_front()?;
        self.open = open;
        Some(open)
    }

    /// Audible reply audio is written to the Echo and not yet played out.
    #[must_use]
    pub(crate) fn voiced(&self) -> bool {
        self.open || self.due().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audible() -> Vec<u8> {
        [2000_i16.to_le_bytes(); 480].concat()
    }

    fn changes(mouth: &mut Mouth, start: Duration, now: Duration) -> Vec<(u128, bool)> {
        let mut seen = Vec::new();
        let mut now = now;
        for _ in 0..500 {
            while let Some(open) = mouth.poll(now) {
                seen.push((now.checked_sub(start).unwrap().as_millis(), open));
            }
            now += Duration::from_millis(10);
        }
        seen
    }

    fn speak(mouth: &mut Mouth, frames: usize, now: Duration) {
        for _ in 0..frames {
            mouth.wrote(&audible(), now);
        }
    }

    #[test]
    fn mouth_stays_open_through_a_gap_shorter_than_the_hold() {
        let start = Duration::ZERO;
        let mut mouth = Mouth::default();
        // 100 ms of speech, 280 ms of quiet, 100 ms of speech: one utterance.
        speak(&mut mouth, 5, start);
        for _ in 0..14 {
            mouth.wrote(&[0; 960], Duration::ZERO);
        }
        speak(&mut mouth, 5, start);
        assert!(mouth.voiced());
        mouth.settle();
        assert_eq!(changes(&mut mouth, start, start), [(80, true), (560, false)]);
        assert!(!mouth.voiced());
    }

    #[test]
    fn mouth_holds_its_close_for_audio_that_may_follow_and_not_after_the_reply_ends() {
        // The reply has more to come: the close waits out the hold, and audio
        // inside it keeps the mouth open.
        let start = Duration::ZERO;
        let mut mouth = Mouth::default();
        speak(&mut mouth, 5, start);
        let now = start + Duration::from_millis(300);
        assert_eq!(mouth.poll(now), Some(true));
        // The speech ended at 180 ms. 120 ms later nothing has closed.
        assert_eq!(mouth.poll(now), None);
        speak(&mut mouth, 5, now);
        mouth.settle();
        assert_eq!(changes(&mut mouth, start, now), [(480, false)]);
        // A quiet of the hold or longer does close the mouth, the hold late.
        let start = Duration::ZERO;
        let mut mouth = Mouth::default();
        speak(&mut mouth, 5, start);
        assert_eq!(
            changes(&mut mouth, start, start),
            [(80, true), (180 + GAP_HOLD.as_millis(), false)]
        );
        // A finished reply closes with its last sound.
        let start = Duration::ZERO;
        let mut mouth = Mouth::default();
        speak(&mut mouth, 5, start);
        mouth.settle();
        assert_eq!(changes(&mut mouth, start, start), [(80, true), (180, false)]);
    }

    fn frame(level: i16) -> Vec<u8> {
        level.to_le_bytes().repeat(480)
    }

    #[test]
    fn mouth_opens_and_closes_on_the_echo_playout_clock_not_on_writes() {
        let mut mouth = Mouth::default();
        let t0 = Duration::ZERO;
        // Silence first, then a lead burst of 5 audible frames written at t0.
        mouth.wrote(&frame(0), t0);
        for _ in 0..5 {
            mouth.wrote(&frame(QUIET.cast_signed() + 1), t0);
        }
        assert_eq!(mouth.due(), Some(t0 + FRAME + OUTPUT_DELAY));
        assert_eq!(mouth.poll(t0 + OUTPUT_DELAY), None);
        assert_eq!(mouth.poll(t0 + FRAME + OUTPUT_DELAY), Some(true));
        // Five audible frames after the leading silence finish at 120 ms. More
        // audio may follow, so the close waits out the gap hold.
        assert_eq!(mouth.due(), Some(t0 + FRAME * 6 + OUTPUT_DELAY + GAP_HOLD));
        // Quiet frames keep the clock running but never reopen or extend it.
        mouth.wrote(&frame(-QUIET.cast_signed()), t0 + FRAME * 3);
        // The reply is complete: the mouth closes with its last sound.
        mouth.settle();
        assert_eq!(mouth.due(), Some(t0 + FRAME * 6 + OUTPUT_DELAY));
        assert_eq!(mouth.poll(t0 + FRAME * 6 + OUTPUT_DELAY), Some(false));
        assert_eq!(mouth.due(), None);
    }

    #[test]
    fn mouth_restarts_the_clock_after_the_echo_ran_dry() {
        let mut mouth = Mouth::default();
        let t0 = Duration::ZERO;
        mouth.wrote(&frame(1000), t0);
        mouth.settle();
        assert_eq!(mouth.poll(t0 + OUTPUT_DELAY), Some(true));
        assert_eq!(mouth.poll(t0 + FRAME + OUTPUT_DELAY), Some(false));
        let later = t0 + Duration::from_secs(3);
        mouth.wrote(&frame(-1000), later);
        assert_eq!(mouth.due(), Some(later + OUTPUT_DELAY));
        assert_eq!(mouth.poll(later + OUTPUT_DELAY), Some(true));
    }

    #[test]
    fn mouth_quiet_thresholds_never_schedule_a_transition() {
        let mut mouth = Mouth::default();
        let t0 = Duration::ZERO;
        for level in [0, 64, -64] {
            mouth.wrote(&frame(level), t0);
        }
        mouth.wrote(&[], t0);
        assert_eq!(mouth.due(), None);
        assert_eq!(mouth.poll(t0 + Duration::from_secs(1)), None);
    }

    #[test]
    fn mouth_preserves_quiet_gaps_of_the_hold_even_when_all_audio_is_written_ahead() {
        let mut mouth = Mouth::default();
        let t0 = Duration::ZERO;
        // One audible frame, 300 ms of quiet, one audible frame.
        mouth.wrote(&frame(65), t0);
        for _ in 0..15 {
            mouth.wrote(&frame(0), t0);
        }
        mouth.wrote(&frame(-65), t0);
        mouth.settle();
        for (offset, open) in [(0, true), (1, false), (16, true), (17, false)] {
            let due = t0 + OUTPUT_DELAY + FRAME * offset;
            assert_eq!(mouth.poll(due.checked_sub(Duration::from_nanos(1)).unwrap()), None);
            assert_eq!(mouth.poll(due), Some(open));
        }
        assert_eq!(mouth.due(), None);
    }
}
