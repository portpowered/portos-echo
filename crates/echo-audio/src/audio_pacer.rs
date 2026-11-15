use crate::FrameQueue;
use std::time::Duration;
use tokio::time::{Instant, sleep_until};

const PERIOD: Duration = Duration::from_millis(20);

/// Single-owner 20 ms frame pacing on Tokio's monotonic clock.
/// Recreate between streams. No task, channel or hidden audio storage is used.
pub struct Pacer {
    deadline: Instant,
}

impl Default for Pacer {
    fn default() -> Self {
        Self::new()
    }
}

impl Pacer {
    /// First deadline is creation +20 ms; requires a Tokio runtime with time.
    #[must_use]
    pub fn new() -> Self {
        Self {
            deadline: Instant::now() + PERIOD,
        }
    }

    /// Wait before popping at most one entry, returning None on underflow.
    /// Cancellation while waiting consumes no entry or deadline. Late polls
    /// emit once and schedule the next deadline after now, skipping catch-up.
    /// Pass only complete frames and do not hold an external lock across await.
    pub async fn next_frame<T>(&mut self, queue: &mut FrameQueue<T>) -> Option<T> {
        sleep_until(self.deadline).await;
        let now = Instant::now();
        self.deadline = if now > self.deadline {
            now + PERIOD
        } else {
            self.deadline + PERIOD
        };
        queue.pop()
    }
}

#[cfg(test)]
mod audio_pacer_tests;
