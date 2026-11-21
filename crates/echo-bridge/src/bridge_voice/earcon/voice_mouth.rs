//! The screen's mouth, timed from the audio written to the Echo.
use crate::bridge_runtime::bridge_pacing::{QUIET, reply_lips};
use std::collections::VecDeque;
use tokio::time::{Duration, Instant};

pub(super) const FRAME: Duration = Duration::from_millis(20);
/// Measured on the real Echo: hassmic reports `played` 0.07 to 0.24 s (typically
/// 0.22 s) after the modelled playout clock below drains.
pub(super) const OUTPUT_DELAY: Duration = Duration::from_millis(200);

/// The screen's mouth follows audio the Echo is actually playing, not model
/// events. Frames written to the Echo play back to back on its clock, which
/// restarts when the device ran dry. The mouth opens when the first audible
/// frame reaches the speaker and closes when the last one has played.
#[derive(Default)]
pub struct Mouth {
    clock: Option<Instant>,
    transitions: VecDeque<(Instant, bool)>,
    open: bool,
}

impl Mouth {
    /// Record one 24 kHz s16le frame written to the Echo at `now`.
    pub fn wrote(&mut self, frame: &[u8], now: Instant) {
        if frame.is_empty() {
            return;
        }
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
                && *due == open
            {
                *due = close;
            } else {
                self.transitions.push_back((open, true));
                self.transitions.push_back((close, false));
            }
        }
    }

    /// When the next open or close is due.
    #[must_use]
    pub fn due(&self) -> Option<Instant> {
        self.transitions.front().map(|(due, _)| *due)
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
