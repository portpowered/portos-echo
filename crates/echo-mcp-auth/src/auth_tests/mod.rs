//! T3 component tests: real HTTP/ES256/files against one local mock per test.
//! The harness keeps fake time manual while network I/O progresses, so runtime
//! auto-advance cannot mistake socket readiness for an expired HTTP deadline.
mod failures;
mod lifecycle;
use crate::mock;
mod protocol;
mod registration;
mod storage;

use super::*;
use mock::Mock;
use std::{future::Future, sync::Arc, task::Poll, time::Duration};
use tokio::time::advance;

async fn drive<F: Future>(future: F) -> F::Output {
    tokio::pin!(future);
    std::future::poll_fn(|cx| {
        match future.as_mut().poll(cx) {
            Poll::Ready(value) => Poll::Ready(value),
            Poll::Pending => {
                // Keep fake time manual while giving the runtime other turns
                // to deliver socket readiness and run the reusable mock.
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    })
    .await
}

async fn until(mut predicate: impl FnMut() -> bool) {
    for _ in 0..100_000 {
        if predicate() {
            return;
        }
        tokio::task::yield_now().await;
    }
    assert!(predicate(), "mock readiness deadline");
}

struct Harness {
    mock: Mock,
    directory: tempfile::TempDir,
    client: Arc<AuthClient>,
}

impl Harness {
    async fn new() -> Self {
        let mock = Mock::start().await;
        mock.set_epoch(1_800_000_000);
        let directory = tempfile::tempdir().unwrap();
        let client = Arc::new(
            drive(AuthClient::open_at_epoch(
                options(&mock, directory.path()),
                1_800_000_000,
            ))
            .await
            .unwrap(),
        );
        Self {
            mock,
            directory,
            client,
        }
    }

    async fn linked() -> Self {
        let harness = Self::new().await;
        let link = drive(harness.client.begin_link()).await.unwrap();
        let task = start_approval(harness.client.clone(), link).await;
        advance(Duration::from_secs(5)).await;
        drive(task).await.unwrap().unwrap();
        harness
    }

    fn record(&self) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(self.directory.path().join("grant.json")).unwrap()).unwrap()
    }

    async fn reload(&self) -> Result<AuthClient, AuthError> {
        let mut client = drive(AuthClient::open(options(&self.mock, self.directory.path()))).await?;
        // A reloaded client shares the harness fake wall clock, including elapsed
        // paused-runtime time; production starts from the current real wall clock.
        client.epoch = self.client.epoch;
        client.started = self.client.started;
        Ok(client)
    }
}

fn options(mock: &Mock, directory: &std::path::Path) -> AuthOptions {
    AuthOptions {
        resource_url: format!("{}/mcp", mock.base),
        client_id: "synthetic-cli".into(),
        scopes: vec!["endpoint:read".into()],
        storage_dir: directory.into(),
        request_timeout: Duration::from_secs(2),
    }
}

fn error<T>(result: Result<T, AuthError>, expected: AuthError) {
    assert_eq!(result.err(), Some(expected));
}

// Acknowledge the first poll only after await_approval registers its sleep.
async fn start_approval(
    client: Arc<AuthClient>,
    link: LinkInstructions,
) -> tokio::task::JoinHandle<Result<(), AuthError>> {
    let (ready, registered) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let mut ready = Some(ready);
        let future = client.await_approval(link);
        tokio::pin!(future);
        std::future::poll_fn(|cx| {
            let result = future.as_mut().poll(cx);
            if let Some(ready) = ready.take() {
                ready.send(()).unwrap();
            }
            result
        })
        .await
    });
    drive(registered).await.unwrap();
    task
}
