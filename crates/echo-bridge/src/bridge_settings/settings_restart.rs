//! A runtime-scoped stop request, observed only at settled voice boundaries.
//! This is admission to cleanup, never permission to exec before cleanup ends.
use std::future::Future;
use tokio::sync::watch;

tokio::task_local! {
    static CURRENT: watch::Receiver<bool>;
}

/// One owner per runtime generation; multiple durable saves coalesce.
/// A fresh owner starts clear, including after loading a saved overlay.
pub struct Restart {
    requested: watch::Sender<bool>,
}

impl Default for Restart {
    fn default() -> Self {
        Self {
            requested: watch::channel(false).0,
        }
    }
}

impl Restart {
    /// Call after a settings transaction has durably committed an effective change.
    pub fn request(&self) {
        self.requested.send_replace(true);
    }

    #[must_use]
    pub fn pending(&self) -> bool {
        *self.requested.borrow()
    }

    /// Scope only the owned service, never unrelated tasks or runtime generations.
    /// The caller still arbitrates shutdown/kill and awaits every resource owner
    /// before replacing the process. A successful voice return alone is not exec.
    pub async fn scope<T>(&self, work: impl Future<Output = T>) -> T {
        CURRENT.scope(self.requested.subscribe(), work).await
    }
}

/// With no home-backed settings owner, keep the existing voice path unchanged.
pub(crate) async fn requested() {
    tokio::select! {
        () = replacement_requested() => (),
        () = super::openai_account::requested() => (),
    }
}

pub(crate) async fn replacement_requested() {
    let Ok(mut receive) = CURRENT.try_with(Clone::clone) else {
        return std::future::pending().await;
    };
    loop {
        if *receive.borrow_and_update() {
            return;
        }
        if receive.changed().await.is_err() {
            return std::future::pending().await;
        }
    }
}

pub(crate) fn replacement_pending() -> bool {
    CURRENT.try_with(|receive| *receive.borrow()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{future::poll_fn, task::Poll};

    async fn pending(work: impl Future<Output = ()>) {
        tokio::pin!(work);
        poll_fn(|cx| {
            assert!(work.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn settings_restart_coalesces_and_never_leaks_into_another_generation() {
        let first = Restart::default();
        first
            .scope(async {
                pending(requested()).await;
                tokio::join!(requested(), async {
                    first.request();
                    first.request();
                });
            })
            .await;
        assert!(first.pending());
        let next = Restart::default();
        next.scope(pending(requested())).await;
        assert!(!next.pending());
        pending(requested()).await;
    }
}
