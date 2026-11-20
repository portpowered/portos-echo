pub(crate) mod jobs;
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

pub(crate) async fn start(
    downloads: Downloads,
    core: Arc<Mutex<Core>>,
    stop: watch::Receiver<bool>,
    drain: watch::Receiver<Option<tokio::time::Instant>>,
    results: mpsc::Sender<crate::PackInstallResult>,
    files: Arc<super::Files>,
    mut requests: mpsc::Receiver<jobs::Request>,
) {
    let root = downloads.home.join("screens");
    let client = download::client(downloads.ca.as_deref()).await;
    for (url, digest) in downloads.pins {
        let Some(id) = files.jobs.lock().await.configured() else {
            break;
        };
        let result = execute(
            &root,
            &client,
            jobs::Request { id, url, digest },
            &core,
            &stop,
            &drain,
            &files,
        )
        .await;
        tokio::select! {
            biased;
            () = crate::cancelled(stop.clone()) => return,
            () = crate::draining(drain.clone()) => return,
            sent = results.send(result) => if sent.is_err() { break; },
        }
    }
    drop(results);
    loop {
        let request = tokio::select! {
            biased;
            () = crate::cancelled(stop.clone()) => break,
            () = crate::draining(drain.clone()) => break,
            request = requests.recv() => match request { Some(request) => request, None => break },
        };
        let _ = execute(&root, &client, request, &core, &stop, &drain, &files).await;
    }
}

async fn execute(
    root: &std::path::Path,
    client: &Result<reqwest::Client, Error>,
    request: jobs::Request,
    core: &Mutex<Core>,
    stop: &watch::Receiver<bool>,
    drain: &watch::Receiver<Option<tokio::time::Instant>>,
    files: &super::Files,
) -> crate::PackInstallResult {
    files.jobs.lock().await.update(&request.id, "downloading", None);
    let result = if *stop.borrow() || drain.borrow().is_some() || !core.lock().await.accepting() {
        Err(Error::Cancelled)
    } else {
        match client {
            Ok(client) => install(root, client, &request.url, &request.digest, core, stop, drain).await,
            Err(error) => Err(*error),
        }
    };
    files.jobs.lock().await.update(
        &request.id,
        if result.is_ok() { "installed" } else { "failed" },
        result.as_ref().err().copied(),
    );
    match result {
        Ok(id) => crate::PackInstallResult {
            installed: Some(id),
            reason: None,
        },
        Err(error) => crate::PackInstallResult {
            installed: None,
            reason: Some(error.to_string()),
        },
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
) -> Result<String, Error> {
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
