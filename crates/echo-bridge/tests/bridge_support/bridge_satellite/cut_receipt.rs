//! T3: hold the real cut receipt until playback observes satellite EOF.
use super::*;
use echo_link::{
    CutStats, EchoLink, Error, LinkConnector, LinkEvent, LinkEvents, MicFrame, MicrophoneRun, PcmFormat, PlayOutcome,
    RunFailure, RunStats,
};
use std::{
    future::Future,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::Poll,
};
use tokio::sync::Notify;

#[derive(Default)]
struct Gate {
    eof: Notify,
    device: Notify,
    playback: Notify,
    device_first: bool,
    receipt_failure: bool,
    configured_failure: bool,
    issued: AtomicUsize,
    order: std::sync::Mutex<Vec<&'static str>>,
    queued: std::sync::atomic::AtomicBool,
    accepted: std::sync::atomic::AtomicBool,
    device_done: std::sync::atomic::AtomicBool,
    playback_done: std::sync::atomic::AtomicBool,
}
impl Gate {
    async fn producer_eof(&self) {
        let playback = self.playback.notified();
        tokio::pin!(playback);
        playback.as_mut().enable();
        if !self.device_first && !self.playback_done.load(Ordering::SeqCst) {
            playback.await;
        }
        if !self.device_done.swap(true, Ordering::SeqCst) {
            self.order.lock().unwrap().push("device EOF");
            self.device.notify_one();
        }
    }
}
struct Connector<C> {
    inner: C,
    gate: Arc<Gate>,
}
struct Link<L> {
    inner: L,
    gate: Arc<Gate>,
}
struct Events<E>(E, Arc<Gate>);
struct Run<R>(R, Arc<Gate>);
impl<R: MicrophoneRun> MicrophoneRun for Run<R> {
    fn wake_phrase(&self) -> &str {
        self.0.wake_phrase()
    }
    fn stats(&self) -> RunStats {
        self.0.stats()
    }
    async fn recv(&mut self) -> Result<Option<MicFrame>, Error> {
        let result = self.0.recv().await;
        if matches!(result, Ok(None) | Err(_)) {
            self.1.producer_eof().await;
        }
        result
    }
}
impl<E: LinkEvents> LinkEvents for Events<E> {
    type Run = Run<E::Run>;
    async fn recv(&mut self) -> Result<Option<LinkEvent<Self::Run>>, Error> {
        let result = self.0.recv().await;
        if self.1.queued.load(Ordering::SeqCst) && matches!(result, Ok(Some(LinkEvent::Wake(_)))) {
            self.1.eof.notify_one();
        }
        if matches!(result, Ok(None) | Err(_)) {
            self.1.producer_eof().await;
        }
        if matches!(result, Ok(Some(LinkEvent::Played))) {
            self.1.order.lock().unwrap().push("device Played");
        }
        result.map(|event| {
            event.map(|event| match event {
                LinkEvent::Wake(run) => LinkEvent::Wake(Run(run, self.1.clone())),
                LinkEvent::Played => LinkEvent::Played,
                LinkEvent::Mute(value) => LinkEvent::Mute(value),
                LinkEvent::MicrophoneDropped(count) => LinkEvent::MicrophoneDropped(count),
                LinkEvent::Other => LinkEvent::Other,
            })
        })
    }
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
    type Events = Events<L::Events>;
    type ReplySink = L::ReplySink;
    type ReplySource = L::ReplySource;
    fn events(&mut self) -> Result<Self::Events, Error> {
        Ok(Events(self.inner.events()?, self.gate.clone()))
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
    async fn end_run(&self) -> Result<(), Error> {
        self.inner.end_run().await
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
        assert_eq!(
            result,
            if self.gate.configured_failure {
                Ok(PlayOutcome::Cut)
            } else {
                Err(Error::Transport)
            }
        );
        assert!(self.gate.queued.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!self.gate.accepted.load(std::sync::atomic::Ordering::SeqCst));
        if self.gate.device_first {
            self.gate.device.notified().await;
        }
        self.gate.order.lock().unwrap().push("playback result");
        self.gate.playback_done.store(true, Ordering::SeqCst);
        self.gate.playback.notify_waiters();
        // Receipt acceptance requires continued polling of this playback future.
        self.gate.eof.notify_one();
        // The configured precedence case gets a real Played, allowing its
        // producer to finish with retained Unavailable. Inject a distinct
        // playback failure here so a producer-first coordinator cannot pass.
        Err(Error::Transport)
    }
    async fn cut_with_stats(&self) -> Result<CutStats, Error> {
        assert_eq!(self.gate.issued.fetch_add(1, Ordering::SeqCst), 0);
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
        if self.gate.receipt_failure {
            return Err(Error::Timeout);
        }
        self.gate.order.lock().unwrap().push("receipt accepted");
        self.gate.accepted.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(stats)
    }
}

#[tokio::test]
#[ignore = "T3 controlled application and loopback peers"]
async fn bridge_cut_acknowledged_before_disconnect_is_recorded() {
    receipt_case(true, false, false).await;
}
#[tokio::test]
#[ignore = "T3 controlled application and loopback peers"]
async fn bridge_cut_playback_error_before_device_eof_is_recorded() {
    receipt_case(false, false, false).await;
}
#[tokio::test]
#[ignore = "T3 controlled application and loopback peers"]
async fn bridge_cut_secondary_receipt_error_preserves_playback_error() {
    receipt_case(false, true, false).await;
}
#[tokio::test]
#[ignore = "T3 configured application and loopback peers"]
async fn bridge_cut_playback_error_precedes_retained_live_failure() {
    receipt_case(false, false, true).await;
}
#[tokio::test]
#[ignore = "T3 controlled producer failure during pending cut"]
async fn bridge_cut_pending_receipt_producer_error_keeps_control_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let live = bridge_peer::Live::bind_frames(50).await;
    let gate = Arc::new(Gate::default());
    let (mut stream, mut worker, _stop) = start(&path, &live.url, gate.clone(), false).await;
    assert_eq!(read(&mut stream).await.0["type"], "audio-start");
    receive_initial(&mut stream, 10).await;
    interrupt(&live, false).await;
    let sent = 10 + drain_to_stop(&mut stream).await;
    assert!(gate.queued.load(Ordering::SeqCst));
    assert!(!gate.accepted.load(Ordering::SeqCst));
    // A real second Wake fails the producer and releases the issued receipt.
    // Keep the satellite connected without Played: cleanup must not wait for it.
    let row: Value = serde_json::from_str(
        include_str!("../../../../../tests/fixtures/wyoming/wake-word.jsonl")
            .lines()
            .nth(1)
            .unwrap(),
    )
    .unwrap();
    send(&mut stream, "run-pipeline", row["header"]["data"].clone()).await;
    let result = tokio::time::timeout(Duration::from_secs(7), &mut worker).await;
    writeln!(
        std::io::stdout(),
        "producer_error_result={result:?} receipt_accepted={} issued={}",
        gate.accepted.load(Ordering::SeqCst),
        gate.issued.load(Ordering::SeqCst)
    )
    .unwrap();
    if result.is_err() {
        worker.abort();
    }
    assert_eq!(result.unwrap().unwrap(), Err(echo_bridge::Error::Wyoming));
    assert!(gate.accepted.load(Ordering::SeqCst));
    assert_eq!(gate.issued.load(Ordering::SeqCst), 1);
    assert_eq!(*gate.order.lock().unwrap(), ["receipt accepted"]);
    assert_cut_failure(&path, sent, true);
    let entries = trace(&path);
    assert_eq!(entries.iter().filter(|e| e["event"] == "reply.played").count(), 0);
    assert_eq!(entries.iter().filter(|e| e["event"] == "wyoming.run_ended").count(), 0);
    let cut = entries.iter().find(|e| e["event"] == "reply.cut").unwrap();
    assert_eq!(cut["reply_frames"], 49);
    assert_eq!(cut["bridge_evicted"], 0);
}
async fn receipt_case(device_first: bool, receipt_failure: bool, configured: bool) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let live = bridge_peer::Live::bind_frames(50).await;
    let gate = Arc::new(Gate {
        device_first,
        receipt_failure,
        configured_failure: configured,
        ..Gate::default()
    });
    let (mut stream, worker, _stop) = start(&path, &live.url, gate.clone(), configured || receipt_failure).await;
    assert_eq!(read(&mut stream).await.0["type"], "audio-start");
    receive_initial(&mut stream, 10).await;
    interrupt(&live, configured).await;
    let sent = 10 + drain_to_stop(&mut stream).await;
    assert!(gate.queued.load(Ordering::SeqCst));
    assert!(!gate.accepted.load(Ordering::SeqCst));
    if configured {
        send(&mut stream, "played", json!({})).await;
    }
    // Keep the real Played path free of a competing microphone EOF so the
    // configured producer completes with its retained Live failure.
    let peer = if configured {
        Some(stream)
    } else {
        drop(stream);
        None
    };
    assert_eq!(
        tokio::time::timeout(OBSERVATION_TIMEOUT, worker)
            .await
            .unwrap()
            .unwrap(),
        Err(echo_bridge::Error::Wyoming)
    );
    drop(peer);
    assert_observations(&gate, &path, sent, configured);
}
async fn interrupt(live: &bridge_peer::Live, configured: bool) {
    let event = if configured {
        json!({"type":"error", "error":{"type":"server_error","code":"synthetic","message":"synthetic"}})
    } else {
        json!({"type":"response.done","response":{"status":"cancelled"}})
    };
    live.send_acknowledged(event.to_string()).await;
}
async fn start(
    path: &Path,
    live: &str,
    gate: Arc<Gate>,
    configured: bool,
) -> (
    bridge_short::Satellite,
    tokio::task::JoinHandle<Result<(), echo_bridge::Error>>,
    tokio::sync::oneshot::Sender<bool>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut settings = echo_bridge::bridge_input::parse(
        &[],
        [
            Some(address.to_string()),
            Some(live.into()),
            Some(path.to_str().unwrap().into()),
        ],
    )
    .unwrap()
    .unwrap();
    settings.lab = !configured;
    let connector = Connector {
        inner: echo_wyoming::adapter::connector(address, echo_wyoming::ClientOptions::default()),
        gate,
    };
    let (stop, shutdown) = tokio::sync::oneshot::channel();
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
    (bridge_short::Satellite::new(stream), worker, stop)
}
fn assert_observations(gate: &Gate, path: &Path, sent: u16, configured: bool) {
    assert_order(gate);
    let entries = trace(path);
    assert_eq!(entries.iter().filter(|e| e["event"] == "reply.played").count(), 0);
    assert_eq!(entries.iter().filter(|e| e["event"] == "wyoming.run_ended").count(), 0);
    if gate.receipt_failure || configured {
        assert_eq!(entries.iter().filter(|e| e["event"] == "reply.cut").count(), 0);
    } else {
        assert_cut_failure(path, sent, true);
        let cut = entries.iter().find(|e| e["event"] == "reply.cut").unwrap();
        assert_eq!(cut["reply_frames"], 49);
        assert_eq!(cut["bridge_evicted"], 0);
        writeln!(
            std::io::stdout(),
            "cut_accounting sent={sent} dropped={} evicted={} framed={}",
            cut["dropped_frames"],
            cut["bridge_evicted"],
            cut["reply_frames"]
        )
        .unwrap();
    }
    writeln!(
        std::io::stdout(),
        "device_first={} receipt_failure={} configured={configured} issued=1 accepted={} order={:?} sent={sent}",
        gate.device_first,
        gate.receipt_failure,
        !gate.receipt_failure,
        gate.order.lock().unwrap()
    )
    .unwrap();
}
fn assert_order(gate: &Gate) {
    assert_eq!(gate.issued.load(Ordering::SeqCst), 1);
    let order = gate.order.lock().unwrap();
    let eof_order: Vec<_> = order
        .iter()
        .copied()
        .filter(|event| ["playback result", "device EOF"].contains(event))
        .collect();
    if gate.device_first {
        assert_eq!(eof_order, ["device EOF", "playback result"]);
    } else if gate.configured_failure || gate.receipt_failure {
        assert_eq!(eof_order.first(), Some(&"playback result"));
    } else {
        assert_eq!(eof_order, ["playback result", "device EOF"]);
    }
    if gate.configured_failure {
        assert_eq!(order.iter().filter(|event| **event == "device Played").count(), 1);
    }
    if !gate.receipt_failure {
        assert_eq!(order.iter().filter(|event| **event == "receipt accepted").count(), 1);
    }
    assert_eq!(gate.accepted.load(Ordering::SeqCst), !gate.receipt_failure);
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
