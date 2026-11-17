// Coverage-only ownership for children whose raw-profile write can be interrupted.
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn terminated(status: ExitStatus) -> bool {
    #[cfg(windows)]
    {
        status.code() == Some(1) // std::process::Child::kill uses TerminateProcess(..., 1).
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal() == Some(9)
    }
}

pub(super) struct Profiles(PathBuf);

impl Profiles {
    pub fn configure(command: &mut Command) -> Result<Option<Self>, String> {
        let Some(root) = std::env::var_os("ECHO_COVERAGE_CHILD_PROFILES") else {
            return Ok(None);
        };
        let root = PathBuf::from(root);
        std::fs::create_dir_all(&root).map_err(|_| "coverage: cannot create profile root")?;
        let directory = loop {
            let directory = root.join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&directory) {
                Ok(()) => break directory,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err("coverage: cannot create child profile directory".into()),
            }
        };
        command.env("LLVM_PROFILE_FILE", directory.join("child-%p-%m.profraw"));
        Ok(Some(Self(directory)))
    }

    pub fn finish(&self, child: &mut Child) {
        // Classify exits before inspecting bytes. Completed unsuccessful children
        // are natural exits too, and must contribute to report inputs.
        let before = child.try_wait();
        let killed = child.kill().is_ok();
        let after = child.wait();
        let disposition = match (before, after) {
            (Ok(None), Ok(status)) if killed && terminated(status) => "killed",
            (Ok(_), Ok(_)) => "natural",
            _ => "unresolved",
        };
        // Failure leaves the directory unclassified; the collector fails closed.
        let _ = std::fs::write(self.0.join("exit"), disposition);
    }

    pub fn not_started(&self) {
        let _ = std::fs::write(self.0.join("exit"), "not-started");
    }
}
