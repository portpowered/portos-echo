//! T3: hold the real cut receipt until playback observes satellite EOF.
use super::*;
use echo_link::{CutStats, EchoLink, Error, LinkConnector, PcmFormat, PlayOutcome, RunFailure};
use std::{future::Future, num::NonZeroUsize, sync::Arc, task::Poll};
use tokio::sync::Notify;

#[derive(Default)]
struct Gate {
    eof: Notify,
    queued: std::sync::atomic::AtomicBool,
    accepted: std::sync::atomic::AtomicBool,
}
struct Connector<C> {
    inner: C,
    gate: Arc<Gate>,
}
struct Link<L> {
    inner: L,
    gate: Arc<Gate>,
}
impl<C: LinkConnector> LinkConnector for Connector<C> {
    type Link = Link<C::Link>;
    async fn connect(&self) -> Result<Self::Link, Error> {
        Ok(Link {
            inner: self.inner.connect().await?,
            gate: self.gate.clone(),
        })
    }
}
impl<L: EchoLink> EchoLink for Link<L> {
    type Events = L::Events;
    type ReplySink = L::ReplySink;
    type ReplySource = L::ReplySource;
    fn events(&mut self) -> Result<Self::Events, Error> {
        self.inner.events()
    }
    fn reply_channel(&self, capacity: NonZeroUsize) -> (Self::ReplySink, Self::ReplySource) {
        self.inner.reply_channel(capacity)
    }
    async fn enable_capture(&self) -> Result<(), Error> {
        self.inner.enable_capture().await
    }
    async fn probe(&self) -> Result<(), Error> {
        self.inner.probe().await
    }
    async fn pause(&self) -> Result<(), Error> {
        self.inner.pause().await
    }
    async fn fail_run(&self, code: RunFailure) -> Result<(), Error> {
        self.inner.fail_run(code).await
    }
    async fn close(&self) -> Result<(), Error> {
        self.inner.close().await
    }
    async fn play_with_timeout(
        &self,
        format: PcmFormat,
        source: Self::ReplySource,
        deadline: Duration,
    ) -> Result<PlayOutcome, Error> {
        let result = self.inner.play_with_timeout(format, source, deadline).await;
        assert_eq!(result, Err(Error::Transport));
        assert!(self.gate.queued.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!self.gate.accepted.load(std::sync::atomic::Ordering::SeqCst));
        // Make the receipt deliverable exactly as the error wins try_join!.
        self.gate.eof.notify_one();
        result
    }
    async fn cut_with_stats(&self) -> Result<CutStats, Error> {
        let cut = self.inner.cut_with_stats();
        tokio::pin!(cut);
        std::future::poll_fn(|cx| {
            assert!(
                cut.as_mut().poll(cx).is_pending(),
                "request queues before the session runs"
            );
            Poll::Ready(())
        })
        .await;
        self.gate.queued.store(true, std::sync::atomic::Ordering::SeqCst);
        self.gate.eof.notified().await;
        let stats = cut.await?;
        self.gate.accepted.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(stats)
    }
}

#[tokio::test]
#[ignore = "T3 controlled application and loopback peers"]
async fn bridge_cut_acknowledged_before_disconnect_is_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let live = bridge_peer::Live::bind_frames(50).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let settings = echo_bridge::bridge_input::parse(
        &[],
        [
            Some(address.to_string()),
            Some(live.url.clone()),
            Some(path.to_str().unwrap().into()),
        ],
    )
    .unwrap()
    .unwrap();
    let gate = Arc::new(Gate::default());
    let connector = Connector {
        inner: echo_wyoming::adapter::connector(address, echo_wyoming::ClientOptions::default()),
        gate: gate.clone(),
    };
    let (_stop, shutdown) = tokio::sync::oneshot::channel();
    let worker = tokio::spawn(echo_bridge::bridge_runtime::execute_controlled(
        settings,
        echo_bridge::bridge_runtime::Control {
            utc_day: Arc::new(|| 17),
            shutdown,
        },
        connector,
    ));
    let mut stream = listener.accept().await.unwrap().0;
    assert_eq!(bridge_short::read(&mut stream).await.0["type"], "run-satellite");
    assert_eq!(bridge_short::read(&mut stream).await.0["type"], "describe");
    send(&mut stream, "info", json!({"satellite":{"name":"synthetic"}})).await;
    wake(&mut stream).await;
    let mut stream = bridge_short::Satellite::new(stream);
    assert_eq!(read(&mut stream).await.0["type"], "audio-start");
    receive_initial(&mut stream, 10).await;
    live.events
        .send(json!({"type":"response.done","response":{"status":"cancelled"}}).to_string())
        .await
        .unwrap();
    let sent = 10 + drain_to_stop(&mut stream).await;
    assert!(gate.queued.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!gate.accepted.load(std::sync::atomic::Ordering::SeqCst));
    drop(stream);
    assert_eq!(
        tokio::time::timeout(OBSERVATION_TIMEOUT, worker)
            .await
            .unwrap()
            .unwrap(),
        Err(echo_bridge::Error::Wyoming)
    );
    assert_cut_failure(&path, sent, true);
    assert!(gate.accepted.load(std::sync::atomic::Ordering::SeqCst));
}
async fn wake(stream: &mut tokio::net::TcpStream) {
    let row: Value = serde_json::from_str(
        include_str!("../../../../../tests/fixtures/wyoming/wake-word.jsonl")
            .lines()
            .nth(1)
            .unwrap(),
    )
    .unwrap();
    send(stream, "run-pipeline", row["header"]["data"].clone()).await;
}
