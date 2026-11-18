//! Test-only wall-clock observation; never drives the bridge's fake clock.
use std::{io::Write, time::Instant};

#[derive(Clone, Copy)]
pub(super) struct Measurement {
    start: Instant,
    negative: bool,
    emotion: bool,
}

impl Measurement {
    pub(super) fn new(negative: bool, emotion: bool) -> Self {
        Self {
            start: Instant::now(),
            negative,
            emotion,
        }
    }

    pub(super) fn mark(self, phase: &'static str) {
        // Emit each completed boundary immediately so a later aggregate timeout
        // cannot erase the partial/censored observation. Only fixed labels and
        // synthetic case flags are logged, never process output or credentials.
        writeln!(
            std::io::stdout(),
            "M4 phase: {}",
            serde_json::json!({
                "phase": phase, "elapsed_ms": self.start.elapsed().as_secs_f64() * 1000.0,
                "negative": self.negative, "emotion": self.emotion,
            })
        )
        .unwrap();
    }
}
