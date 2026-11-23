use std::{
    sync::OnceLock,
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Now {
    pub monotonic: Duration,
    pub wall: SystemTime,
}

pub trait Clock {
    fn now(&self) -> Now;
}

/// Host wall time, including when called from a simulation task.
pub struct SystemClock;

impl Clock for SystemClock {
    #[allow(clippy::disallowed_methods, reason = "Host clock implementation")]
    fn now(&self) -> Now {
        static START: OnceLock<Instant> = OnceLock::new();
        Now {
            monotonic: START.get_or_init(Instant::now).elapsed(),
            wall: SystemTime::now(),
        }
    }
}

/// A clock that advances only when the scenario advances it.
pub struct PausedClock(Now);

impl PausedClock {
    #[must_use]
    pub const fn new(wall: SystemTime) -> Self {
        Self(Now {
            monotonic: Duration::ZERO,
            wall,
        })
    }

    /// Advances both clock domains. An unrepresentable wall time leaves both unchanged.
    pub fn advance(&mut self, by: Duration) {
        if let Some(wall) = self.0.wall.checked_add(by) {
            self.0 = Now {
                monotonic: self.0.monotonic.saturating_add(by),
                wall,
            };
        }
    }
}

impl Clock for PausedClock {
    fn now(&self) -> Now {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn paused_clock_advances_both_domains_from_a_pre_epoch_time() {
        let start = UNIX_EPOCH - Duration::from_secs(2);
        let mut clock = PausedClock::new(start);
        assert_eq!(clock.now().monotonic, Duration::ZERO);
        assert_eq!(
            clock
                .now()
                .wall
                .duration_since(UNIX_EPOCH)
                .unwrap_err()
                .duration()
                .as_secs(),
            2
        );
        clock.advance(Duration::from_millis(3500));
        assert_eq!(clock.now().monotonic, Duration::from_millis(3500));
        assert_eq!(clock.now().wall.duration_since(UNIX_EPOCH).unwrap().as_secs(), 1);
    }
}
