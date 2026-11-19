//! Reuse the operator kill marker; never acquire or mutate credentials here.
use super::ServeNotice;
use crate::{Error, bridge_runtime::bridge_kill::inspect};
use std::{cell::Cell, path::Path, time::Duration};

/// The terminating run keeps its cause even if EOF arrives during cleanup or
/// the operator clears the marker. A newly admitted run starts fresh.
#[derive(Default)]
pub(super) struct Termination {
    active: Cell<bool>,
    killed: Cell<bool>,
    reported: Cell<bool>,
    failed: Cell<bool>,
}

impl Termination {
    pub fn observe(&self, notice: &ServeNotice) {
        match notice {
            ServeNotice::Listening => {
                self.active.set(true);
                self.killed.set(false);
                self.reported.set(false);
                self.failed.set(false);
            }
            ServeNotice::Kill(true) if self.active.get() => {
                self.trip();
                self.reported.set(true);
            }
            ServeNotice::Session(super::SessionNotice::Completed(_)) | ServeNotice::Ready => self.active.set(false),
            ServeNotice::Session(super::SessionNotice::Run(super::Notice::Device(echo_link::LinkEvent::Mute(
                true,
            )))) if self.active.get() => self.failed.set(true),
            _ => (),
        }
    }

    pub fn trip(&self) {
        if self.active.get() {
            self.killed.set(true);
        }
    }

    /// The kill switch ended the active run; final once `finish` has run.
    pub fn killed(&self) -> bool {
        self.killed.get()
    }

    pub fn returned(&self, result: &Result<(), Error>, requested: bool) {
        if result.is_err() && !requested {
            self.failed.set(true);
        }
    }

    pub fn finish(
        &self,
        result: Result<(), Error>,
        requested: bool,
        path: Option<&Path>,
        observe: &mut impl FnMut(ServeNotice) -> Result<(), Error>,
    ) -> Result<(), Error> {
        // Host/device/process cleanup has finished. This is the final read:
        // EOF may have won the select before the service's next marker poll.
        if self.active.get() && !self.killed.get() && path.is_some_and(|path| inspect(path).unwrap_or(true)) {
            self.trip();
        }
        if self.killed.get() && !self.reported.get() {
            observe(ServeNotice::Kill(true))?;
            self.reported.set(true);
        }
        if self.killed.get() {
            Err(Error::Control)
        } else if result == Err(Error::Control) && requested && !self.failed.get() {
            Ok(())
        } else {
            result
        }
    }
}

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

    pub async fn cancel(&self, shutdown: impl std::future::Future<Output = ()>, killed: &Cell<bool>) {
        tokio::select! {
            biased;
            () = self.wait() => killed.set(true),
            () = shutdown => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_completion_keeps_kill_authority_until_run_completion() {
        for ended in [false, true] {
            let termination = Termination::default();
            termination.observe(&ServeNotice::Listening);
            termination.observe(&ServeNotice::Session(super::super::SessionNotice::ReplyCompleted(
                super::super::Stats::default(),
            )));
            if ended {
                termination.observe(&ServeNotice::Session(super::super::SessionNotice::Completed(
                    super::super::Stats::default(),
                )));
            }
            termination.trip();
            let mut publications = 0;
            let result = termination.finish(Ok(()), false, None, &mut |notice| {
                assert!(matches!(notice, ServeNotice::Kill(true)));
                publications += 1;
                Ok(())
            });
            assert_eq!(result, if ended { Ok(()) } else { Err(Error::Control) });
            assert_eq!(publications, usize::from(!ended));
        }
    }

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
