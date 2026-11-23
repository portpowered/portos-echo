use std::{
    future::Future,
    pin::Pin,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(super) type Wait<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

pub(super) trait Clock: Send + Sync + 'static {
    fn now(&self) -> u64;
    fn wait_until(&self, deadline: u64) -> Wait<'_>;
}

pub(super) struct EpochClock;

impl Clock for EpochClock {
    fn now(&self) -> u64 {
        since_epoch().as_secs()
    }

    fn wait_until(&self, deadline: u64) -> Wait<'_> {
        Box::pin(async move {
            loop {
                let remaining = Duration::from_secs(deadline).saturating_sub(since_epoch());
                if remaining.is_zero() {
                    return;
                }
                tokio::time::sleep(remaining.min(Duration::from_secs(3600))).await;
            }
        })
    }
}

#[allow(clippy::disallowed_methods, reason = "Existing host time")]
fn since_epoch() -> Duration {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default()
}

#[cfg(test)]
pub(super) use manual::ManualClock;

#[cfg(test)]
mod manual {
    use tokio::sync::watch;

    use super::{Clock, Wait};

    #[derive(Clone)]
    pub(crate) struct ManualClock(watch::Sender<u64>);

    impl ManualClock {
        pub(crate) fn new(now: u64) -> Self {
            Self(watch::Sender::new(now))
        }

        pub(crate) fn set(&self, now: u64) {
            self.0.send_replace(now);
        }
    }

    impl Clock for ManualClock {
        fn now(&self) -> u64 {
            *self.0.borrow()
        }

        fn wait_until(&self, deadline: u64) -> Wait<'_> {
            Box::pin(async move {
                let _ = self.0.subscribe().wait_for(|now| *now >= deadline).await;
            })
        }
    }
}
