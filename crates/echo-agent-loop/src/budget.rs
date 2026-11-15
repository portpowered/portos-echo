use crate::lock;
use std::{sync::Mutex, time::SystemTime};

/// Instance-local UTC accounting. A dispatched attempt is never refunded.
pub(crate) struct Budget {
    state: Mutex<(u64, u64)>,
    cap: u64,
    day: Box<dyn Fn() -> u64 + Send + Sync>,
}

impl Budget {
    pub(crate) fn new(cap: u64) -> Self {
        Self::with_day(cap, || {
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                / 86_400
        })
    }

    fn with_day(cap: u64, day: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        Self {
            state: Mutex::new((day(), 0)),
            cap,
            day: Box::new(day),
        }
    }

    pub(crate) fn reserve(&self) -> Result<(), &'static str> {
        let mut state = lock(&self.state);
        let day = (self.day)();
        if state.0 != day {
            *state = (day, 0);
        }
        if state.1 >= self.cap {
            return Err("daily request limit reached");
        }
        state.1 += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    #[test]
    fn atomic_last_reservation_and_injected_utc_rollover() {
        let day = Arc::new(AtomicU64::new(7));
        let source = day.clone();
        let budget = Arc::new(Budget::with_day(1, move || source.load(Ordering::SeqCst)));
        let workers: Vec<_> = (0..16)
            .map(|_| {
                let budget = budget.clone();
                std::thread::spawn(move || budget.reserve())
            })
            .collect();
        let successes = workers.into_iter().flat_map(|worker| worker.join().unwrap()).count();
        assert_eq!(successes, 1);
        assert_eq!(budget.reserve(), Err("daily request limit reached"));
        day.store(8, Ordering::SeqCst);
        assert_eq!(budget.reserve(), Ok(()));
        assert_eq!(budget.reserve(), Err("daily request limit reached"));
        assert_eq!(Budget::new(0).reserve(), Err("daily request limit reached"));
    }
}
