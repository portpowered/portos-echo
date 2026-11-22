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
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn wait_until(&self, deadline: u64) -> Wait<'_> {
        Box::pin(async move {
            loop {
                let seconds = deadline.saturating_sub(self.now());
                if seconds == 0 {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(seconds.min(3600))).await;
            }
        })
    }
}
