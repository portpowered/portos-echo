//! A credential failure latches refusal until explicit sign-in and restart.
use super::bridge_screen::Screen;
use crate::{Error, bridge_health::Health, bridge_trace::Trace};
use echo_link::{EchoLink, ReplySink, RunFailure};
use serde_json::json;
use std::{
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub(super) fn cause(error: &Error) -> RunFailure {
    if *error == Error::Authorization {
        RunFailure::RelinkRequired
    } else {
        RunFailure::Unavailable
    }
}

/// One turn owns both the first actual PCM observation and its refusal decision.
pub(super) struct RunLink<'a, L> {
    pub client: &'a L,
    started: Arc<AtomicBool>,
    refused: AtomicBool,
    error: Mutex<Option<echo_link::Error>>,
    pub kill: Option<&'a super::bridge_kill::Kill<'a>>,
    pub lab: bool,
}

impl<'a, L: EchoLink> RunLink<'a, L> {
    pub fn new(client: &'a L) -> Self {
        Self {
            client,
            started: Arc::new(AtomicBool::new(false)),
            refused: AtomicBool::new(false),
            error: Mutex::new(None),
            kill: None,
            lab: false,
        }
    }

    pub async fn refuse(&self, cause: RunFailure) -> Result<(), Error> {
        self.notify(cause).await.map_err(Error::from)
    }

    pub async fn failure(
        &self,
        error: Error,
        context: (bool, &Health, &mut Trace, &Screen, &str),
    ) -> Result<(), Error> {
        let reason = cause(&error);
        let (lab, health, trace, screen, id) = context;
        failure(error, lab, health, trace, screen, id).await?;
        self.refuse(reason).await
    }

    async fn notify(&self, cause: RunFailure) -> Result<(), echo_link::Error> {
        if self.started.load(Ordering::Acquire) {
            return Ok(());
        }
        if self.refused.swap(true, Ordering::AcqRel) {
            return self
                .error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .map_or(Ok(()), Err);
        }
        let result = self.client.fail_run(cause).await;
        if let Err(error) = result {
            *self.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
        }
        result
    }
}

pub(super) struct ObservedSink<S> {
    sink: S,
    started: Arc<AtomicBool>,
}

impl<S: ReplySink> ReplySink for ObservedSink<S> {
    async fn send(&self, frame: Vec<u8>) -> Result<(), echo_link::Error> {
        let nonempty = !frame.is_empty();
        self.sink.send(frame).await?;
        if nonempty {
            self.started.store(true, Ordering::Release);
        }
        Ok(())
    }
}

impl<L: EchoLink> EchoLink for RunLink<'_, L> {
    type Events = L::Events;
    type ReplySink = ObservedSink<L::ReplySink>;
    type ReplySource = L::ReplySource;
    fn events(&mut self) -> Result<Self::Events, echo_link::Error> {
        Err(echo_link::Error::Busy)
    }
    fn reply_channel(&self, capacity: NonZeroUsize) -> (Self::ReplySink, Self::ReplySource) {
        let (sink, source) = self.client.reply_channel(capacity);
        (
            ObservedSink {
                sink,
                started: self.started.clone(),
            },
            source,
        )
    }
    async fn enable_capture(&self) -> Result<(), echo_link::Error> {
        self.client.enable_capture().await
    }
    async fn probe(&self) -> Result<(), echo_link::Error> {
        self.client.probe().await
    }
    async fn play_with_timeout(
        &self,
        format: echo_link::PcmFormat,
        source: Self::ReplySource,
        deadline: Duration,
    ) -> Result<echo_link::PlayOutcome, echo_link::Error> {
        self.client.play_with_timeout(format, source, deadline).await
    }
    async fn cut_with_stats(&self) -> Result<echo_link::CutStats, echo_link::Error> {
        let stats = self.client.cut_with_stats().await?;
        // Settle playback through its existing receiver, but notify the device
        // before the journey waits for Played. Never cancel the play future.
        if !self.lab && self.kill.is_some_and(super::bridge_kill::Kill::enabled) {
            self.notify(RunFailure::KillSwitch).await?;
        }
        Ok(stats)
    }
    async fn fail_run(&self, cause: RunFailure) -> Result<(), echo_link::Error> {
        self.client.fail_run(cause).await
    }
    async fn pause(&self) -> Result<(), echo_link::Error> {
        self.client.pause().await
    }
    async fn close(&self) -> Result<(), echo_link::Error> {
        self.client.close().await
    }
}

pub(super) async fn failure(
    error: Error,
    lab: bool,
    health: &Health,
    trace: &mut Trace,
    screen: &Screen,
    id: &str,
) -> Result<(), Error> {
    if let Error::Mcp(reason) = &error {
        trace.record(
            "mcp.run.failed",
            Some(id),
            json!({"reason":crate::bridge_mcp::category(reason)}),
        )?;
        return Ok(());
    }
    health.state.send_modify(|state| state.live = "error");
    // The synthetic lab caller retains its existing fatal-error protocol.
    if lab
        || !matches!(
            error,
            Error::Authorization | Error::Unavailable | Error::Deadline | Error::Decode
        )
    {
        return Err(error);
    }
    let relink = error == Error::Authorization;
    health.state.send_modify(|state| state.relink_required = relink);
    complete_publication(
        screen.publish_confirmed(super::bridge_screen::failure_state(relink)),
        trace,
        id,
        relink,
        error,
    )
    .await
}

async fn complete_publication(
    publication: impl std::future::Future<Output = Result<(), Error>>,
    trace: &mut Trace,
    id: &str,
    relink: bool,
    error: Error,
) -> Result<(), Error> {
    if let Err(error) = publication.await {
        trace.record("run.failed", Some(id), json!({"reason":error.to_string()}))?;
        return Err(error);
    }
    trace.record(
        if relink { "auth.relink_required" } else { "run.failed" },
        Some(id),
        json!({"reason":error.to_string()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;

    #[tokio::test]
    async fn bridge_relink_completion_waits_for_publication() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let mut trace = Trace::open(&path, "api-key").unwrap();
        let (release, held) = tokio::sync::oneshot::channel();
        let completion = complete_publication(
            async { held.await.unwrap() },
            &mut trace,
            "run-1",
            true,
            Error::Authorization,
        );
        tokio::pin!(completion);
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(completion.as_mut().poll(cx).is_pending())).await);
        assert_eq!(std::fs::read(&path).unwrap(), b"");
        release.send(Ok(())).unwrap();
        assert_eq!(completion.await, Ok(()));
        let entries = std::fs::read_to_string(&path).unwrap();
        assert_eq!(entries.lines().count(), 1);
        let entry: serde_json::Value = serde_json::from_str(&entries).unwrap();
        assert_eq!(entry["event"], "auth.relink_required");
        assert_eq!(entry["reason"], "authorization");
    }

    #[tokio::test]
    async fn bridge_relink_publication_failure_never_claims_relink() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let mut trace = Trace::open(&path, "api-key").unwrap();
        assert_eq!(
            complete_publication(
                async { Err(Error::Screen) },
                &mut trace,
                "run-1",
                true,
                Error::Authorization
            )
            .await,
            Err(Error::Screen)
        );
        let entry: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(entry["event"], "run.failed");
        assert_eq!(entry["reason"], "screen");
        assert_eq!(
            complete_publication(async { Ok(()) }, &mut trace, "run-1", false, Error::Unavailable).await,
            Ok(())
        );
        assert_eq!(
            complete_publication(async { Ok(()) }, &mut trace, "run-1", true, Error::Decode).await,
            Ok(())
        );
    }

    #[tokio::test]
    async fn bridge_relink_trace_failure_is_propagated() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let mut trace = Trace::open(&path, "api-key").unwrap();
        for publication in [Ok(()), Err(Error::Screen)] {
            assert_eq!(
                complete_publication(
                    async { publication },
                    &mut trace,
                    &"x".repeat(4096),
                    true,
                    Error::Authorization
                )
                .await,
                Err(Error::Trace)
            );
        }
        assert_eq!(std::fs::read(path).unwrap(), b"");
    }

    #[tokio::test]
    async fn bridge_relink_no_screen_and_fatal_branches_keep_semantics() {
        let directory = tempfile::tempdir().unwrap();
        let mut trace = Trace::open(&directory.path().join("trace"), "api-key").unwrap();
        let health = Health::offline();
        let screen = Screen::bind(None).await.unwrap();
        assert_eq!(
            failure(Error::Authorization, true, &health, &mut trace, &screen, "run-1").await,
            Err(Error::Authorization)
        );
        assert_eq!(
            failure(Error::Screen, false, &health, &mut trace, &screen, "run-1").await,
            Err(Error::Screen)
        );
        assert_eq!(
            failure(Error::Authorization, false, &health, &mut trace, &screen, "run-1").await,
            Ok(())
        );
        assert_eq!(
            failure(
                Error::Mcp(echo_mcp_auth::AuthError::RelinkRequired),
                false,
                &health,
                &mut trace,
                &screen,
                "run-1"
            )
            .await,
            Ok(())
        );
        assert!(health.state.borrow().relink_required);
        assert_eq!(health.state.borrow().live, "error");
    }
}

#[cfg(test)]
mod run_tests {
    use super::*;
    use echo_link::{
        LinkConnector,
        fake::{Command, FakeConnector, Observer},
    };

    #[tokio::test(start_paused = true)]
    async fn run_failure_once_per_turn_and_closed_link_never_retries_old_error() {
        let observer = Observer::default();
        let client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        for cause in [
            RunFailure::RelinkRequired,
            RunFailure::KillSwitch,
            RunFailure::Unavailable,
        ] {
            let link = RunLink::new(&client);
            link.refuse(cause).await.unwrap();
            link.refuse(RunFailure::Unavailable).await.unwrap();
        }
        assert_eq!(
            observer.commands(),
            vec![
                Command::Connect,
                Command::FailRun(RunFailure::RelinkRequired),
                Command::FailRun(RunFailure::KillSwitch),
                Command::FailRun(RunFailure::Unavailable)
            ]
        );
        let link = RunLink::new(&client);
        link.close().await.unwrap();
        assert_eq!(link.refuse(RunFailure::Unavailable).await.err(), Some(Error::Wyoming));
        assert_eq!(link.refuse(RunFailure::Unavailable).await.err(), Some(Error::Wyoming));
        assert_eq!(observer.commands().last(), Some(&Command::Close));
        assert_eq!(cause(&Error::Authorization), RunFailure::RelinkRequired);
        assert_eq!(cause(&Error::Deadline), RunFailure::Unavailable);
    }

    #[tokio::test(start_paused = true)]
    async fn run_failure_empty_audio_is_unserved_but_actual_pcm_preserves_cut_and_ack() {
        let observer = Observer::default();
        let client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut link = RunLink::new(&client);
        assert!(matches!(link.events(), Err(echo_link::Error::Busy)));
        link.enable_capture().await.unwrap();
        link.probe().await.unwrap();
        let (sink, source) = link.reply_channel(NonZeroUsize::new(2).unwrap());
        sink.send(vec![]).await.unwrap();
        link.refuse(RunFailure::Unavailable).await.unwrap();
        drop(source);
        assert_eq!(sink.send(vec![1, 2]).await, Err(echo_link::Error::Closed));
        let link = RunLink::new(&client);
        let (sink, source) = link.reply_channel(NonZeroUsize::new(2).unwrap());
        sink.send(vec![1, 2]).await.unwrap();
        link.refuse(RunFailure::Unavailable).await.unwrap();
        drop(sink);
        let play = link.play_with_timeout(
            echo_link::PcmFormat {
                rate: 24000,
                width: 2,
                channels: 1,
            },
            source,
            Duration::from_secs(1),
        );
        let control = async {
            tokio::task::yield_now().await;
            link.cut_with_stats().await.unwrap();
            observer.finish(Ok(()));
        };
        let (result, ()) = tokio::join!(play, control);
        assert_eq!(result, Ok(echo_link::PlayOutcome::Cut));
        assert_eq!(
            observer
                .commands()
                .iter()
                .filter(|command| matches!(command, Command::FailRun(_)))
                .count(),
            1
        );
        link.pause().await.unwrap();
        link.fail_run(RunFailure::KillSwitch).await.unwrap();
        link.close().await.unwrap();
    }
}
