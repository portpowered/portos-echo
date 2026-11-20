mod active;
pub(crate) mod jobs;
mod notification;
pub(crate) mod settings;
use super::{Error, archive, download};
use crate::screen_core::Core;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, mpsc, watch};

pub(crate) struct Downloads {
    pub home: PathBuf,
    pub ca: Option<PathBuf>,
    pub pins: Vec<(String, String)>,
}

// A dropped controller future also cancels its synchronous preparation. The
// owned worker always awaits that task before returning from shutdown.
struct Cancellation(Arc<AtomicBool>);
impl Drop for Cancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

struct Worker {
    root: PathBuf,
    client: Result<reqwest::Client, Error>,
    core: Arc<Mutex<Core>>,
    stop: watch::Receiver<bool>,
    drain: watch::Receiver<Option<tokio::time::Instant>>,
    files: Arc<super::Files>,
}

pub(crate) async fn start(
    downloads: Downloads,
    core: Arc<Mutex<Core>>,
    stop: watch::Receiver<bool>,
    drain: watch::Receiver<Option<tokio::time::Instant>>,
    results: mpsc::Sender<crate::PackInstallResult>,
    files: Arc<super::Files>,
    requests: mpsc::Receiver<jobs::Request>,
) {
    let worker = Worker {
        root: downloads.home.join("screens"),
        client: download::client(downloads.ca.as_deref()).await,
        core,
        stop,
        drain,
        files,
    };
    tokio::join!(
        async {
            worker.configured(downloads.pins, results).await;
            worker.interactive(requests).await;
        },
        worker.watch_settings(),
        worker.watch_active(),
    );
}

impl Worker {
    async fn configured(&self, pins: Vec<(String, String)>, results: mpsc::Sender<crate::PackInstallResult>) {
        for (url, digest) in pins {
            let Some(id) = self.files.jobs.lock().await.configured() else {
                break;
            };
            let result = self.execute(jobs::Request { id, url, digest }).await;
            tokio::select! {
                biased;
                () = crate::cancelled(self.stop.clone()) => return,
                () = crate::draining(self.drain.clone()) => return,
                sent = results.send(result) => if sent.is_err() { break; },
            }
        }
    }

    async fn interactive(&self, mut requests: mpsc::Receiver<jobs::Request>) {
        loop {
            let request = tokio::select! {
                biased;
                () = crate::cancelled(self.stop.clone()) => break,
                () = crate::draining(self.drain.clone()) => break,
                request = requests.recv() => match request { Some(request) => request, None => break },
            };
            let _ = self.execute(request).await;
        }
    }

    async fn execute(&self, request: jobs::Request) -> crate::PackInstallResult {
        self.files.jobs.lock().await.update(&request.id, "downloading", None);
        let result = if *self.stop.borrow() || self.drain.borrow().is_some() || !self.core.lock().await.accepting() {
            Err(Error::Cancelled)
        } else {
            match &self.client {
                Ok(client) => {
                    install(
                        &self.root,
                        client,
                        &request.url,
                        &request.digest,
                        &self.core,
                        &self.stop,
                        &self.drain,
                    )
                    .await
                }
                Err(error) => Err(*error),
            }
        };
        if matches!(&result, Ok((_, true))) {
            self.notify().await;
        }
        self.finish(&request.id, result).await
    }

    async fn finish(&self, job: &str, result: Result<(String, bool), Error>) -> crate::PackInstallResult {
        self.files.jobs.lock().await.update(
            job,
            if result.is_ok() { "installed" } else { "failed" },
            result.as_ref().err().copied(),
        );
        match result {
            Ok((id, _)) => crate::PackInstallResult {
                installed: Some(id),
                reason: None,
            },
            Err(error) => crate::PackInstallResult {
                installed: None,
                reason: Some(error.to_string()),
            },
        }
    }
}
async fn install(
    root: &std::path::Path,
    client: &reqwest::Client,
    url: &str,
    digest: &str,
    core: &Mutex<Core>,
    stop: &watch::Receiver<bool>,
    drain: &watch::Receiver<Option<tokio::time::Instant>>,
) -> Result<(String, bool), Error> {
    download::url(url)?;
    tokio::fs::create_dir_all(root).await.map_err(|_| Error::Storage)?;
    let metadata = tokio::fs::symlink_metadata(root).await.map_err(|_| Error::Storage)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(Error::Storage);
    }
    let compressed = tempfile::Builder::new()
        .prefix(".pack-download-")
        .tempfile_in(root)
        .map_err(|_| Error::Storage)?;
    let cancellation = Cancellation(Arc::new(AtomicBool::new(false)));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let future = download::fetch(client, url, compressed.path());
    tokio::select! {
        biased;
        () = crate::cancelled(stop.clone()) => return Err(Error::Cancelled),
        () = crate::draining(drain.clone()) => return Err(Error::Cancelled),
        result = tokio::time::timeout_at(deadline, future) => result.map_err(|_| Error::Deadline)??,
    }
    let root_owned = root.to_owned();
    let digest = digest.to_owned();
    let cancelled = cancellation.0.clone();
    let mut task =
        tokio::task::spawn_blocking(move || archive::prepare(&root_owned, compressed.path(), &digest, &cancelled));
    let prepared = tokio::select! {
        biased;
        () = crate::cancelled(stop.clone()) => None,
        () = crate::draining(drain.clone()) => None,
        () = tokio::time::sleep_until(deadline) => None,
        result = &mut task => Some(result.map_err(|_| Error::Storage)??),
    };
    let Some(prepared) = prepared else {
        cancellation.0.store(true, Ordering::Release);
        let _ = task.await;
        return Err(if tokio::time::Instant::now() >= deadline {
            Error::Deadline
        } else {
            Error::Cancelled
        });
    };
    let core = core.lock().await;
    if !core.accepting() || *stop.borrow() || drain.borrow().is_some() {
        return Err(Error::Cancelled);
    }
    prepared.publish(root, &cancellation.0)
}
