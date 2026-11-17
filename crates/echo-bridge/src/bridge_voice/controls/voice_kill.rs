//! Reuse the operator kill marker; never acquire or mutate credentials here.
use super::ServeNotice;
use crate::{Error, bridge_runtime::bridge_kill::inspect};
use std::{path::Path, time::Duration};

pub(super) struct Gate<'a> {
    path: Option<&'a Path>,
    enabled: bool,
}

impl<'a> Gate<'a> {
    pub fn new(path: Option<&'a Path>) -> Self {
        Self { path, enabled: false }
    }

    fn read(&self) -> bool {
        self.path.is_some_and(|path| inspect(path).unwrap_or(true))
    }

    pub fn sync(&mut self, observe: &mut impl FnMut(ServeNotice) -> Result<(), Error>) -> Result<bool, Error> {
        let enabled = self.read();
        if self.enabled != enabled {
            observe(ServeNotice::Kill(enabled))?;
            self.enabled = enabled;
        }
        Ok(enabled)
    }

    pub fn tripped(&mut self, observe: &mut impl FnMut(ServeNotice) -> Result<(), Error>) -> Result<(), Error> {
        if !self.enabled {
            observe(ServeNotice::Kill(true))?;
            self.enabled = true;
        }
        Ok(())
    }

    pub async fn wait(&self) {
        loop {
            if self.read() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latched_kill_survives_clear_during_cleanup_and_failed_publication_is_not_committed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("kill");
        let mut gate = Gate::new(Some(&path));
        let mut notices = Vec::new();
        let mut observe = |notice| {
            if let ServeNotice::Kill(value) = notice {
                notices.push(value);
            }
            Ok(())
        };
        assert!(!gate.sync(&mut observe).unwrap());
        // A wait observed kill, but it cleared before cleanup returned. The
        // latched observation must publish before the following clear.
        gate.tripped(&mut observe).unwrap();
        gate.tripped(&mut observe).unwrap();
        assert!(!gate.sync(&mut observe).unwrap());
        assert_eq!(notices, [true, false]);
        assert_eq!(gate.tripped(&mut |_| Err(Error::Screen)), Err(Error::Screen));
        assert!(!gate.enabled);
        std::fs::write(&path, b"invalid marker").unwrap();
        assert!(gate.sync(&mut |_| Ok(())).unwrap());
    }
}
