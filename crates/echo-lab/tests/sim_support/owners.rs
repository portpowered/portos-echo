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
        assert_eq!(
            observed["owner_completed"], true,
            "actual hassmic listener owner survived"
        );
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub struct Completion<T> {
    name: &'static str,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<T>,
}

impl<T: Send + 'static> Completion<T> {
    pub fn new(name: &'static str, shutdown: impl Future<Output = T> + Send + 'static) -> Self {
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = stopped.await;
            shutdown.await
        });
        Self {
            name,
            stop: Some(stop),
            task,
        }
    }

    pub fn stop(&mut self) {
        self.stop.take().unwrap().send(()).unwrap();
    }

    pub async fn assert_closed(mut self) -> T {
        // Observe under the existing nextest test ceiling; the joined API
        // retains its original per-owner cleanup deadlines.
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), &mut self.task).await;
        if result.is_err() {
            if self.stop.is_some() {
                self.stop();
                let _ = (&mut self.task).await;
            } else {
                self.task.abort();
                let _ = (&mut self.task).await;
            }
        }
        assert!(result.is_ok(), "{} listener owners did not complete", self.name);
        result.unwrap().unwrap()
    }
}

#[tokio::test(start_paused = true)]
async fn t3_completed_owner_cannot_discharge_another_live_owners_barrier() {
    let first = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let second = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = second.local_addr().unwrap();
    let mut completed = Completion::new("completed peer", async move { drop(first) });
    let retained = Completion::new("retained peer", async move { drop(second) });
    completed.stop();
    completed.assert_closed().await;
    let _connection = tokio::net::TcpStream::connect(address).await.unwrap();
    let failure = tokio::spawn(retained.assert_closed()).await.unwrap_err();
    assert!(failure.is_panic());
    let message = failure.into_panic().downcast::<String>().unwrap();
    assert!(message.contains("retained peer listener owners did not complete"));
}
