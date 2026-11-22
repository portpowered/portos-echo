use std::{
    future::Future,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(super) trait Clock: Send + Sync + 'static {
    fn now(&self) -> u64;
    fn wait_until(&self, deadline: u64) -> impl Future<Output = ()> + Send;
}

pub(super) struct EpochClock;

impl Clock for EpochClock {
    fn now(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    async fn wait_until(&self, deadline: u64) {
        loop {
            let seconds = deadline.saturating_sub(self.now());
            if seconds == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_secs(seconds.min(3600))).await;
        }
    }
}
