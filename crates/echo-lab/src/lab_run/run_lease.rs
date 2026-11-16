//! Monotonic lease budget. Reserve a command slot for teardown; never extend a hold.
use std::time::{Duration, Instant};

const LIFETIME_MS: u64 = 660_000;
const COMMAND: Duration = Duration::from_secs(30);

pub struct LeaseClock {
    opened_ms: u64,
    observed_ms: u64,
    deadline: Option<Instant>,
}

impl LeaseClock {
    pub fn new() -> Self {
        Self {
            opened_ms: 0,
            observed_ms: 0,
            deadline: None,
        }
    }

    pub fn observe(&mut self, now: u64) -> Result<(), String> {
        if now < self.observed_ms {
            return Err("clock: monotonic time moved backwards".into());
        }
        self.observed_ms = now;
        if let Some(deadline) = self.deadline {
            let remaining = LIFETIME_MS.saturating_sub(now.saturating_sub(self.opened_ms));
            self.deadline = Some(deadline.min(Instant::now() + Duration::from_millis(remaining)));
        }
        Ok(())
    }

    pub fn start(&mut self) {
        self.opened_ms = self.observed_ms;
        self.deadline = Some(Instant::now() + Duration::from_millis(LIFETIME_MS));
    }

    pub fn clear(&mut self) {
        self.deadline = None;
    }

    pub fn renew(&self) -> bool {
        // A playback plus WAV processing can consume two 30s slots; leave
        // another 30s for release. Rotate before starting work that cannot fit.
        self.deadline
            .is_some_and(|end| end.saturating_duration_since(Instant::now()) <= COMMAND * 3)
    }

    pub fn deadline(&self, cleanup: bool) -> Instant {
        let limit = Instant::now() + COMMAND;
        self.deadline.map_or(limit, |end| {
            let end = if cleanup {
                end
            } else {
                end.checked_sub(COMMAND).unwrap_or(end)
            };
            limit.min(end)
        })
    }
}

/// A queued clip retains its deadline until the host confirms eligibility.
#[derive(Default)]
pub struct PlaybackGate {
    pub pending: Option<String>,
}

impl PlaybackGate {
    pub fn acknowledge(&mut self, reached: bool) -> Result<(), String> {
        if !reached {
            return Err("playback wait: completion not reached".into());
        }
        self.pending = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_gate_retains_pending_end_until_confirmed() {
        let mut gate = PlaybackGate::default();
        assert_eq!(gate.pending, None);
        gate.pending = Some("2026-11-15T09:00:00.500Z".into());
        assert_eq!(
            gate.acknowledge(false),
            Err("playback wait: completion not reached".into())
        );
        assert_eq!(gate.pending.as_deref(), Some("2026-11-15T09:00:00.500Z"));
        assert_eq!(gate.acknowledge(true), Ok(()));
        assert_eq!(gate.pending, None);
        gate.pending = Some(String::new());
        assert!(gate.acknowledge(false).is_err());
        assert_eq!(gate.pending, Some(String::new()));
    }

    #[test]
    fn budgets_never_extend_and_clock_regressions_fail_closed() {
        let mut clock = LeaseClock::new();
        clock.observe(1_000).unwrap();
        clock.start();
        assert!(!clock.renew());
        clock.observe(660_000).unwrap();
        assert!(clock.renew());
        assert!(clock.deadline(true) <= Instant::now() + Duration::from_secs(1));
        assert!(clock.deadline(false) < Instant::now());
        assert_eq!(clock.observe(0), Err("clock: monotonic time moved backwards".into()));
        clock.clear();
        assert!(!clock.renew());
    }
}
