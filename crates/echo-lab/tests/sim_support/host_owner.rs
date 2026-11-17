// Shared private T3 adapters; no product exports and no address-based proofs.
use std::path::{Path, PathBuf};

pub struct Host {
    root: PathBuf,
    python: PathBuf,
}

impl Host {
    pub fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let directory = format!(
            "lab-owner-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let root = repo.join(".cache").join(&directory);
        std::fs::create_dir_all(root.parent().unwrap()).unwrap();
        std::fs::create_dir(&root).unwrap();
        let adapter = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lab_run/sim/host_tests/owner.py");
        let python = root.join(if cfg!(windows) { "python.cmd" } else { "python" });
        let script = if cfg!(windows) {
            format!("@echo off\r\npython -u \"{}\" {directory} %*\r\n", adapter.display())
        } else {
            format!(
                "#!/bin/sh\nexec python3 -u '{}' {directory} \"$@\"\n",
                adapter.display()
            )
        };
        std::fs::write(&python, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self { root, python }
    }

    pub fn python(&self) -> &str {
        self.python.to_str().unwrap()
    }

    pub fn assert_closed(&self) {
        let observed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.root.join("host.json")).unwrap()).unwrap();
        let observations = observed.as_array().unwrap();
        assert!(!observations.is_empty(), "host owner observation never started");
        for observation in observations {
            assert_eq!(
                observation["owner_completed"], true,
                "actual hassmic listener owner survived"
            );
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
