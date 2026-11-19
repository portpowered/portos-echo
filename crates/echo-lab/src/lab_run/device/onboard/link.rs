//! hassmic's own log says whether a wake can reach the bridge beside it.
use super::{POLL, Remote};
use crate::lab_run::device::{Configured, runtime};
use std::time::Duration;

const CONNECTED: [&str; 2] = ["client connected", "satellite: running"];
const DISCONNECTED: &str = "client disconnected";
const NO_LINK: &str = "wake word: no Home Assistant link, nothing started";
const UNLINKED: &str = "device: unmet prerequisite: hassmic link to the bridge on the Echo; its log shows no \
\"client connected\" or \"satellite: running\" after the last \"client disconnected\"";
const WAKE_UNLINKED: &str =
    "device: wake without a hassmic link; hassmic logged \"wake word: no Home Assistant link, nothing started\"";
const LINK_WAIT: Duration = Duration::from_secs(15);

pub(super) fn linked(log: &str) -> bool {
    let up = CONNECTED.iter().filter_map(|marker| log.rfind(marker)).max();
    up.is_some_and(|up| log.rfind(DISCONNECTED).is_none_or(|down| down < up))
}

impl Remote {
    pub(super) fn log_size(&self) -> Result<u64, String> {
        self.shell(&format!("wc -c {}", self.launcher.hassmic_log))?
            .split_whitespace()
            .next()
            .and_then(|size| size.parse().ok())
            .ok_or_else(|| "device: unmet prerequisite: readable hassmic log on the Echo".into())
    }

    // A log shorter than the mark was rotated: all of it is new.
    fn log_since(&self, mark: u64) -> Result<String, String> {
        let from = if self.log_size()? < mark { 1 } else { mark + 1 };
        let bytes = self.read(&format!("tail -c +{from} {}", self.launcher.hassmic_log))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub(super) fn mark_link(&mut self) -> Result<(), String> {
        self.link_mark = self.log_size()?;
        Ok(())
    }

    /// Only lines written since the bridge was last started, or hassmic last killed, count.
    pub(super) fn linked(&self) -> Result<bool, String> {
        Ok(linked(&self.log_since(self.link_mark)?))
    }

    pub(super) async fn wait_link(&self) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + LINK_WAIT;
        loop {
            if self.linked()? {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(UNLINKED.into());
            }
            tokio::time::sleep(POLL).await;
        }
    }

    fn wake_linked(&self) -> Result<(), String> {
        if self.log_since(self.wake_mark)?.contains(NO_LINK) {
            return Err(WAKE_UNLINKED.into());
        }
        Ok(())
    }
}

impl runtime::Runtime {
    /// Before a wake plays, hassmic must show the bridge linked.
    pub(in crate::lab_run::device) async fn before_wake(&mut self) -> Result<(), String> {
        let Some(remote) = self.remote.as_mut() else {
            return Ok(());
        };
        remote.wait_link().await?;
        remote.wake_mark = remote.log_size()?;
        Ok(())
    }

    /// A wake hassmic heard with no link is a named failure, not a timeout.
    pub(in crate::lab_run::device) fn wake_linked(&self) -> Result<(), String> {
        self.remote.as_ref().map_or(Ok(()), Remote::wake_linked)
    }
}

impl Configured<'_> {
    pub(in crate::lab_run::device) fn before_wake(&mut self) -> Result<(), String> {
        self.executor.block_on(runtime::controlled(
            self.state.before_wake(),
            self.options.cancel.as_deref(),
        ))
    }

    pub(in crate::lab_run::device) fn wait_wake(&mut self, index: usize) -> Result<(), String> {
        self.state.wake_linked()?;
        match self.wait_played(index) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.state.wake_linked()?;
                Err(error)
            }
        }
    }
}
