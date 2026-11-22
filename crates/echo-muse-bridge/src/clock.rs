use std::{
    future::Future,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Absolute epoch seconds, including readiness-driven waiting at a deadline.
pub trait Clock: Send + Sync {
    fn now(&self) -> u64;
    fn wait_until(&self, deadline: u64) -> impl Future<Output = ()> + Send;
}

pub struct EpochClock;

impl Clock for EpochClock {
    #[allow(clippy::disallowed_methods, reason = "Existing host time")]
    fn now(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    async fn wait_until(&self, deadline: u64) {
        while self.now() < deadline {
            tokio::time::sleep(Duration::from_secs(deadline.saturating_sub(self.now()))).await;
        }
    }
}
