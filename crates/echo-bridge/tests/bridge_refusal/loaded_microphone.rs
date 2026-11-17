//! T3: production bounded Wyoming capture and call owner, controlled loopback peers.
use super::continuous::settings;
use super::*;
use echo_link::{
    CutStats, EchoLink, LinkConnector, LinkEvent, LinkEvents, MicFrame, MicrophoneRun, PcmFormat, PlayOutcome,
    ReplySink, RunFailure, RunStats,
};
use std::{
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

#[derive(Default)]
struct Schedule {
    competing: AtomicBool,
    reply_pending: AtomicBool,
    reply_polls: AtomicUsize,
    cut_pending: AtomicBool,
    cut_polls: AtomicUsize,
    retiring: AtomicBool,
    retirement_polls: AtomicUsize,
    blocked: AtomicBool,
    turns: AtomicUsize,
    run: Mutex<Option<echo_wyoming::Run>>,
    delivered: Mutex<Vec<u16>>,
}
struct Connector(std::net::SocketAddr, Arc<Schedule>);
struct Link(echo_wyoming::WyomingClient, Arc<Schedule>);
struct Events(echo_wyoming::adapter::Events, Arc<Schedule>);
struct Run(Arc<Schedule>);
struct Sink(echo_wyoming::adapter::Sink, Arc<Schedule>);

impl ReplySink for Sink {
    async fn send(&self, frame: Vec<u8>) -> Result<(), echo_link::Error> {
        self.1.reply_polls.fetch_add(1, Ordering::SeqCst);
        std::future::poll_fn(|cx| {
            if self.1.reply_pending.load(Ordering::SeqCst) {
                self.1.reply_polls.fetch_add(1, Ordering::SeqCst);
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            } else {
                std::task::Poll::Ready(())
            }
        })
        .await;
        self.0.send(frame).await
    }
}
impl Schedule {
    fn stats(&self) -> echo_wyoming::RunStats {
        self.run.lock().unwrap().as_ref().unwrap().stats()
    }
}
impl LinkConnector for Connector {
    type Link = Link;
    async fn connect(&self) -> Result<Link, echo_link::Error> {
        Ok(Link(
            echo_wyoming::WyomingClient::connect(self.0, echo_wyoming::ClientOptions::default())
                .await
                .unwrap(),
            self.1.clone(),
        ))
    }
}
impl LinkEvents for Events {
    type Run = Run;
    async fn recv(&mut self) -> Result<Option<LinkEvent<Run>>, echo_link::Error> {
        if self.1.competing.load(Ordering::SeqCst) {
            // Model a ready backlog of payload-free device notifications. The
            // cooperative budget lets the independently gated wire producer run.
            tokio::task::consume_budget().await;
            self.1.turns.fetch_add(1, Ordering::SeqCst);
            return Ok(Some(LinkEvent::Other));
        }
        Ok(self.0.recv().await?.map(|event| match event {
            LinkEvent::Wake(run) => {
                *self.1.run.lock().unwrap() = Some(run);
                self.1.competing.store(true, Ordering::SeqCst);
                LinkEvent::Wake(Run(self.1.clone()))
            }
            LinkEvent::Played => LinkEvent::Played,
            LinkEvent::Mute(value) => LinkEvent::Mute(value),
            LinkEvent::MicrophoneDropped(count) => LinkEvent::MicrophoneDropped(count),
            LinkEvent::Other => LinkEvent::Other,
        }))
    }
}
impl MicrophoneRun for Run {
    fn wake_phrase(&self) -> &'static str {
        "synthetic"
    }
    async fn recv(&mut self) -> Result<Option<MicFrame>, echo_link::Error> {
        let frame = std::future::poll_fn(|cx| {
            if self.0.blocked.load(Ordering::SeqCst) {
                return std::task::Poll::Pending;
            }
            let mut run = self.0.run.lock().unwrap();
            let receive = MicrophoneRun::recv(run.as_mut().unwrap());
            std::pin::pin!(receive).poll(cx)
        })
        .await?;
        if let Some(frame) = &frame {
            self.0
                .delivered
                .lock()
                .unwrap()
                .push(u16::from_le_bytes([frame.payload[0], frame.payload[1]]));
        }
        Ok(frame)
    }
    fn stats(&self) -> RunStats {
        let stats = self.0.stats();
        RunStats {
            received: stats.received,
            dropped: stats.dropped,
        }
    }
}
impl EchoLink for Link {
    type Events = Events;
    type ReplySink = Sink;
    type ReplySource = tokio::sync::mpsc::Receiver<Vec<u8>>;
    fn events(&mut self) -> Result<Events, echo_link::Error> {
        Ok(Events(EchoLink::events(&mut self.0)?, self.1.clone()))
    }
    fn reply_channel(&self, size: NonZeroUsize) -> (Self::ReplySink, Self::ReplySource) {
        let (tx, rx) = self.0.reply_channel(size);
        (Sink(tx, self.1.clone()), rx)
    }
    async fn enable_capture(&self) -> Result<(), echo_link::Error> {
        self.0.enable_capture().await
    }
    async fn probe(&self) -> Result<(), echo_link::Error> {
        self.0.probe().await
    }
    async fn play_with_timeout(
        &self,
        format: PcmFormat,
        source: Self::ReplySource,
        deadline: Duration,
    ) -> Result<PlayOutcome, echo_link::Error> {
        let outcome = EchoLink::play_with_timeout(&self.0, format, source, deadline).await;
        while self.1.retiring.load(Ordering::SeqCst) {
            self.1.retirement_polls.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
        }
        outcome
    }
    async fn cut_with_stats(&self) -> Result<CutStats, echo_link::Error> {
        let stats = EchoLink::cut_with_stats(&self.0).await?;
        std::future::poll_fn(|cx| {
            self.1.cut_polls.fetch_add(1, Ordering::SeqCst);
            if self.1.cut_pending.load(Ordering::SeqCst) {
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            } else {
                std::task::Poll::Ready(())
            }
        })
        .await;
        Ok(stats)
    }
    async fn fail_run(&self, code: RunFailure) -> Result<(), echo_link::Error> {
        self.0.fail_run(code).await
    }
    async fn pause(&self) -> Result<(), echo_link::Error> {
        EchoLink::pause(&self.0).await
    }
    async fn close(&self) -> Result<(), echo_link::Error> {
        EchoLink::close(&self.0).await
    }
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn offer(peer: &mut tokio::net::TcpStream, index: u16) {
    let mut payload = vec![0; 640];
    payload[..2].copy_from_slice(&index.to_le_bytes());
    peer.write_all(&echo_contracts::wyoming::encode(&echo_contracts::wyoming::Event {
        header: echo_contracts::wyoming::Header {
            r#type: "audio-chunk".into(),
            data: Some(json!({"rate":16000,"width":2,"channels":1,"timestamp":index})),
            data_length: 0,
            payload_length: 0,
        },
        data: None,
        payload,
    }))
    .await
    .unwrap();
}
#[tokio::test]
#[ignore = "T3 production runtime, bounded capture and local Live/Wyoming sockets"]
async fn loaded_microphone_device_notifications_do_not_starve_bounded_capture() {
    exercise(false).await;
}
#[tokio::test]
#[ignore = "T3 production runtime, bounded capture and local Live/Wyoming sockets"]
async fn loaded_microphone_genuine_overload_keeps_newest_and_exact_drops() {
    exercise(true).await;
}
async fn exercise(blocked: bool) {
    let directory = tempfile::tempdir().unwrap();
    let live = bridge_peer::Live::bind_frames(0).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let schedule = Arc::new(Schedule::default());
    schedule.blocked.store(blocked, Ordering::SeqCst);
    let (stop, shutdown) = tokio::sync::oneshot::channel();
    let runtime = tokio::spawn(echo_bridge::bridge_runtime::execute_controlled(
        settings(directory.path(), &live.url),
        echo_bridge::bridge_runtime::Control {
            utc_day: Arc::new(|| 0),
            shutdown,
        },
        Connector(listener.local_addr().unwrap(), schedule.clone()),
    ));
    let mut peer = listener.accept().await.unwrap().0;
    start_capture(&mut peer, directory.path(), &runtime).await;
    offer_sequence(&mut peer, &schedule).await;
    schedule.blocked.store(false, Ordering::SeqCst);
    schedule.competing.store(false, Ordering::SeqCst);
    until(|| schedule.delivered.lock().unwrap().len() as u64 + schedule.stats().dropped == 64).await;
    let stats = schedule.stats();
    let delivered = schedule.delivered.lock().unwrap().clone();
    // Retire and join even on the failing-before schedule, then check the oracle.
    stop.send(true).unwrap();
    assert_eq!(runtime.await.unwrap(), Ok(()));
    assert_delivery(blocked, stats, &delivered);
}
fn assert_delivery(blocked: bool, stats: echo_wyoming::RunStats, delivered: &[u16]) {
    assert_eq!(stats.received, 64);
    let dropped = if blocked { 32 } else { 0 };
    assert_eq!(
        stats.dropped, dropped,
        "ready device events starved bounded capture: {delivered:?}"
    );
    assert_eq!(delivered, &(u16::try_from(dropped).unwrap()..64).collect::<Vec<_>>());
    writeln!(std::io::stdout(), "loaded microphone: offered=64 received=64 delivered={} dropped={dropped} ordered={}..64 capacity=32 blocked={blocked}", delivered.len(), dropped).unwrap();
}
async fn start_capture<T>(peer: &mut tokio::net::TcpStream, directory: &Path, runtime: &tokio::task::JoinHandle<T>) {
    assert_eq!(bridge_short::read(peer).await.0["type"], "run-satellite");
    assert_eq!(bridge_short::read(peer).await.0["type"], "describe");
    bridge_short::send(peer, "info", json!({})).await;
    bridge_wire_auth_failure::trace_wait::observe(&directory.join("trace"), "wyoming.connected", || {
        format!("in-process bridge task finished={}", runtime.is_finished())
    })
    .await;
    bridge_short::send(
        peer,
        "run-pipeline",
        json!({
            "start_stage":"asr", "end_stage":"tts", "restart_on_end":false, "wake_word_name":"synthetic"
        }),
    )
    .await;
    assert_eq!(bridge_short::read_reply(peer).await.0["type"], "audio-start");
}
async fn offer_sequence(peer: &mut tokio::net::TcpStream, schedule: &Schedule) {
    for index in 0..64 {
        offer(peer, index).await;
        until(|| schedule.stats().received == u64::from(index) + 1).await;
        // Same producer schedule before/after: eight competing device turns
        // per chunk, irrespective of whether the microphone consumer advanced.
        let turns = schedule.turns.load(Ordering::SeqCst);
        until(|| schedule.turns.load(Ordering::SeqCst) >= turns + 8).await;
    }
}

#[tokio::test]
#[ignore = "T3 production runtime, bounded capture and local Live/Wyoming sockets"]
async fn loaded_microphone_pending_reply_admission_keeps_capture_serviced() {
    let directory = tempfile::tempdir().unwrap();
    let live = bridge_peer::Live::bind_frames(3).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let schedule = Arc::new(Schedule::default());
    schedule.reply_pending.store(true, Ordering::SeqCst);
    let (stop, shutdown) = tokio::sync::oneshot::channel();
    let runtime = tokio::spawn(echo_bridge::bridge_runtime::execute_controlled(
        settings(directory.path(), &live.url),
        echo_bridge::bridge_runtime::Control {
            utc_day: Arc::new(|| 0),
            shutdown,
        },
        Connector(listener.local_addr().unwrap(), schedule.clone()),
    ));
    let mut peer = listener.accept().await.unwrap().0;
    start_capture(&mut peer, directory.path(), &runtime).await;
    // Reply readiness is withheld, never microphone readiness. Eight owner
    // polls per independently received chunk expose the service gap without
    // elapsed-time assumptions or a deliberately blocked capture consumer.
    until(|| schedule.reply_polls.load(Ordering::SeqCst) > 0).await;
    poll_sequence(&mut peer, &schedule, 0, &schedule.reply_polls).await;
    schedule.reply_pending.store(false, Ordering::SeqCst);
    schedule.competing.store(false, Ordering::SeqCst);
    until(|| schedule.delivered.lock().unwrap().len() as u64 + schedule.stats().dropped == 64).await;
    let stats = schedule.stats();
    let delivered = schedule.delivered.lock().unwrap().clone();
    stop.send(true).unwrap();
    assert_eq!(runtime.await.unwrap(), Ok(()));
    assert_delivery(false, stats, &delivered);
}

#[tokio::test]
#[ignore = "T3 production runtime, bounded capture and local Live/Wyoming sockets"]
async fn loaded_microphone_pending_cut_receipt_keeps_capture_serviced() {
    let directory = tempfile::tempdir().unwrap();
    let live = bridge_peer::Live::bind_frames(3).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let schedule = Arc::new(Schedule::default());
    schedule.cut_pending.store(true, Ordering::SeqCst);
    let (stop, shutdown) = tokio::sync::oneshot::channel();
    let runtime = tokio::spawn(echo_bridge::bridge_runtime::execute_controlled(
        settings(directory.path(), &live.url),
        echo_bridge::bridge_runtime::Control {
            utc_day: Arc::new(|| 0),
            shutdown,
        },
        Connector(listener.local_addr().unwrap(), schedule.clone()),
    ));
    let mut peer = listener.accept().await.unwrap().0;
    start_capture(&mut peer, directory.path(), &runtime).await;
    bridge_wire_auth_failure::trace_wait::observe(&directory.path().join("trace"), "reply.started", || {
        format!("in-process bridge task finished={}", runtime.is_finished())
    })
    .await;
    live.events
        .send(json!({"type":"response.done","response":{"status":"cancelled"}}).to_string())
        .await
        .unwrap();
    until(|| schedule.cut_polls.load(Ordering::SeqCst) > 0).await;
    poll_sequence(&mut peer, &schedule, 0, &schedule.cut_polls).await;
    schedule.cut_pending.store(false, Ordering::SeqCst);
    schedule.competing.store(false, Ordering::SeqCst);
    until(|| schedule.delivered.lock().unwrap().len() as u64 + schedule.stats().dropped == 64).await;
    let stats = schedule.stats();
    let delivered = schedule.delivered.lock().unwrap().clone();
    let packets = delivered.len();
    bridge_short::send(&mut peer, "audio-stop", json!({})).await;
    bridge_short::send(&mut peer, "played", json!({})).await;
    until(|| live.microphone_frames.load(Ordering::SeqCst) == packets).await;
    stop.send(true).unwrap();
    schedule.competing.store(false, Ordering::SeqCst);
    assert_eq!(runtime.await.unwrap(), Ok(()));
    writeln!(std::io::stdout(), "pending cut: offered=0..64 received={} delivered={delivered:?} offered_samples=20480 delivered_samples={} dropped={} fake_received={packets} capacity=32", stats.received, delivered.len()*320, stats.dropped).unwrap();
    assert_delivery(false, stats, &delivered);
}

#[tokio::test]
#[ignore = "T3 production owner retirement, real bounded Wyoming and loopback Live"]
async fn loaded_microphone_playback_retirement_drains_finished_capture() {
    retirement(true).await;
}

#[tokio::test]
#[ignore = "T3 production owner retirement, real bounded Wyoming and loopback Live"]
async fn loaded_microphone_playback_retirement_forwards_active_capture() {
    retirement(false).await;
}

async fn retirement(finished: bool) {
    let directory = tempfile::tempdir().unwrap();
    let live = bridge_peer::Live::bind_frames(3).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let schedule = Arc::new(Schedule::default());
    schedule.retiring.store(true, Ordering::SeqCst);
    schedule.reply_pending.store(finished, Ordering::SeqCst);
    let (stop, shutdown) = tokio::sync::oneshot::channel();
    let runtime = tokio::spawn(echo_bridge::bridge_runtime::execute_controlled(
        settings(directory.path(), &live.url),
        echo_bridge::bridge_runtime::Control {
            utc_day: Arc::new(|| 0),
            shutdown,
        },
        Connector(listener.local_addr().unwrap(), schedule.clone()),
    ));
    let mut peer = listener.accept().await.unwrap().0;
    start_capture(&mut peer, directory.path(), &runtime).await;
    let initial = if finished { 32 } else { 0 };
    prepare_retirement(&mut peer, &schedule, &live, initial).await;
    poll_sequence(&mut peer, &schedule, initial, &schedule.retirement_polls).await;
    schedule.retiring.store(false, Ordering::SeqCst);
    bridge_short::send(&mut peer, "audio-stop", json!({})).await;
    finish_retirement(&mut peer, &live, initial).await;
    bridge_wire_auth_failure::trace_wait::observe(&directory.path().join("trace"), "mic.completed", || {
        format!("runtime finished={}", runtime.is_finished())
    })
    .await;
    stop.send(true).unwrap();
    assert_eq!(runtime.await.unwrap(), Ok(()));
    assert_retirement(&schedule, &live, &directory.path().join("trace"), initial).await;
}

async fn assert_retirement(schedule: &Schedule, live: &bridge_peer::Live, path: &Path, initial: u16) {
    let entries = trace(path);
    let mic = entries.iter().find(|e| e["event"] == "mic.completed").unwrap();
    assert_eq!(schedule.stats().received, u64::from(initial) + 64);
    assert_eq!(schedule.stats().dropped, 0, "retirement evicted ready capture: {mic}");
    assert_eq!(
        schedule.delivered.lock().unwrap().as_slice(),
        &(0..initial + 64).collect::<Vec<u16>>()
    );
    let active = if initial == 0 { 64 } else { 32 };
    for (field, expected) in [
        ("count", active),
        ("input_samples", active * 320),
        ("tail_samples", 0),
        ("dropped_chunks", 0),
    ] {
        assert_eq!(mic[field], expected, "{mic}");
    }
    until(|| live.microphone_frames.load(Ordering::SeqCst) >= active).await;
    assert_eq!(live.microphone_frames.load(Ordering::SeqCst), active);
    assert_eq!(entries.iter().filter(|e| e["event"] == "reply.cut").count(), 1);
    assert_eq!(
        entries
            .iter()
            .filter(|e| e["event"] == "reply.played" && e["outcome"] == "cut")
            .count(),
        1
    );
    assert_eq!(
        entries
            .iter()
            .filter(|e| e["event"] == "reply.played" && e["outcome"] == "played")
            .count(),
        usize::from(initial == 0)
    );
    writeln!(
        std::io::stdout(),
        "playback retirement: received={} drained={} dropped0 active{active} samples{} fake{active}",
        initial + 64,
        initial + 64,
        active * 320
    )
    .unwrap();
}

async fn poll_sequence(peer: &mut tokio::net::TcpStream, schedule: &Schedule, start: u16, counter: &AtomicUsize) {
    for index in start..start + 64 {
        offer(peer, index).await;
        until(|| schedule.stats().received == u64::from(index) + 1).await;
        // Independent wire receipt and eight owner opportunities, never consumption.
        let polls = counter.load(Ordering::SeqCst);
        until(|| counter.load(Ordering::SeqCst) >= polls + 8).await;
    }
}

async fn prepare_retirement(
    peer: &mut tokio::net::TcpStream,
    schedule: &Schedule,
    live: &bridge_peer::Live,
    initial: u16,
) {
    for index in 0..initial {
        offer(peer, index).await;
        until(|| schedule.delivered.lock().unwrap().len() == usize::from(index) + 1).await;
    }
    until(|| schedule.reply_polls.load(Ordering::SeqCst) > 0).await;
    if initial > 0 {
        live.events
            .send(json!({"type":"response.audio.done"}).to_string())
            .await
            .unwrap();
    }
    live.events
        .send(json!({"type":"response.done","response":{"status":"cancelled"}}).to_string())
        .await
        .unwrap();
    until(|| schedule.cut_polls.load(Ordering::SeqCst) > 0).await;
    schedule.competing.store(false, Ordering::SeqCst);
    while bridge_short::read_reply(peer).await.0["type"] != "audio-stop" {}
    bridge_short::send(peer, "played", json!({})).await;
    until(|| schedule.retirement_polls.load(Ordering::SeqCst) > 0).await;
}

async fn finish_retirement(peer: &mut tokio::net::TcpStream, live: &bridge_peer::Live, initial: u16) {
    if initial > 0 {
        while bridge_short::read(peer).await.0["type"] != "describe" {}
        bridge_short::send(peer, "info", json!({})).await;
    } else {
        assert_eq!(bridge_short::read_reply(peer).await.0["type"], "audio-start");
        until(|| live.microphone_frames.load(Ordering::SeqCst) == 64).await;
        live.events
            .send(json!({"type":"response.audio.done"}).to_string())
            .await
            .unwrap();
        while bridge_short::read_reply(peer).await.0["type"] != "audio-stop" {}
        bridge_short::send(peer, "played", json!({})).await;
    }
}
