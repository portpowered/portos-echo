//! Native host orchestration. Device lifecycle policy belongs to echo-install.sh.
mod adb;
pub mod cli;
pub mod doctor;
pub mod install;
pub mod integrity;
pub mod lifecycle;
mod release;
pub mod setup;
mod version;
pub mod wifi;

/// Fixed public diagnostics never contain dependency output or caller data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failure(pub &'static str);

pub type Result<T> = std::result::Result<T, Failure>;

/// Authored lifecycle and release helper travel as one stdin program.
pub const DEVICE_SCRIPT: &str = concat!(
    include_str!("../../../scripts/echo-install-setup.sh"),
    "\n",
    include_str!("../../../scripts/echo-install-release.sh"),
    "\n",
    include_str!("../../../scripts/echo-install.sh")
);

/// Latched orderly setup cancellation. Cleanup is awaited by each resource owner.
#[derive(Clone)]
pub struct Cancellation(tokio::sync::watch::Sender<bool>);

impl Default for Cancellation {
    fn default() -> Self {
        Self(tokio::sync::watch::channel(false).0)
    }
}

impl Cancellation {
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }

    /// Cancel only read/local work. Never wrap lifecycle mutation or cleanup.
    /// # Errors
    /// Returns a fixed resumable error when cancellation wins.
    pub(crate) async fn until<T>(&self, work: impl std::future::Future<Output = Result<T>>) -> Result<T> {
        let mut cancelled = self.0.subscribe();
        tokio::select! {
            biased;
            _ = cancelled.wait_for(|value| *value) => Err(Failure("setup_cancelled")),
            result = Box::pin(work) => result,
        }
    }
}
