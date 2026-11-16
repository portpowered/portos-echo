use crate::AuthError;
use std::{
    future::Future,
    pin::Pin,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(crate) trait Clock: Send + Sync {
    fn unix(&self) -> Result<u64, AuthError>;
    fn monotonic(&self) -> Duration;
    fn wait(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}
pub(crate) struct SystemClock(tokio::time::Instant);
impl SystemClock {
    pub(crate) fn new() -> Self {
        Self(tokio::time::Instant::now())
    }
}
impl Clock for SystemClock {
    fn unix(&self) -> Result<u64, AuthError> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|t| t.as_secs())
            .map_err(|_| AuthError::InvalidCredential)
    }
    fn monotonic(&self) -> Duration {
        self.0.elapsed()
    }
    fn wait(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(duration))
    }
}
