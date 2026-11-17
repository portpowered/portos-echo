//! T3 functional: production owner and loopback Live, inexhaustible device input.
use super::*;
use echo_link::{
    CutStats, EchoLink, LinkConnector, LinkEvent, LinkEvents, MicFrame, MicrophoneRun, PcmFormat, PlayOutcome,
    RunFailure, RunStats,
    fake::{self, FakeConnector, Observer},
};
use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

#[derive(Clone, Default)]
pub(crate) struct Input {
    pub observer: Observer,
    pub frames: Arc<AtomicUsize>,
    stopped: Arc<AtomicBool>,
}
pub(crate) struct Link(fake::FakeLink, Input);
pub(crate) struct Events(fake::Events, Input, bool);
pub(crate) struct Run(Input);

impl Input {
    pub fn wake(&self) {
        self.observer.wake("synthetic", vec![]);
    }
}
impl LinkConnector for Input {
    type Link = Link;
    async fn connect(&self) -> Result<Link, echo_link::Error> {
        Ok(Link(
            FakeConnector::new(self.observer.clone()).connect().await?,
            self.clone(),
        ))
    }
}
impl LinkEvents for Events {
    type Run = Run;
    async fn recv(&mut self) -> Result<Option<LinkEvent<Run>>, echo_link::Error> {
        // The configured owner requires the initial successful probe's Info event.
        if std::mem::take(&mut self.2) {
            return Ok(Some(LinkEvent::Other));
        }
        Ok(self.0.recv().await?.map(|event| match event {
            LinkEvent::Wake(_) => LinkEvent::Wake(Run(self.1.clone())),
            LinkEvent::Played => LinkEvent::Played,
            LinkEvent::Mute(value) => LinkEvent::Mute(value),
            LinkEvent::MicrophoneDropped(value) => LinkEvent::MicrophoneDropped(value),
            LinkEvent::Other => LinkEvent::Other,
        }))
    }
}
impl MicrophoneRun for Run {
    fn wake_phrase(&self) -> &'static str {
        "synthetic"
    }
    fn recv(&mut self) -> impl std::future::Future<Output = Result<Option<MicFrame>, echo_link::Error>> + Send {
        if self.0.stopped.load(Ordering::SeqCst) {
            return std::future::ready(Ok(None));
        }
        let count = self.0.frames.fetch_add(1, Ordering::SeqCst);
        assert!(
            count < 10_000,
            "continuous-input operation guard: control never progressed"
        );
        std::future::ready(Ok(Some(MicFrame {
            format: PcmFormat {
                rate: 16_000,
                width: 2,
                channels: 1,
            },
            payload: vec![0; 640],
        })))
    }
    fn stats(&self) -> RunStats {
        RunStats {
            received: self.0.frames.load(Ordering::SeqCst) as u64,
            dropped: 0,
        }
    }
}
impl EchoLink for Link {
    type Events = Events;
    type ReplySink = fake::Sink;
    type ReplySource = fake::Source;
    fn events(&mut self) -> Result<Events, echo_link::Error> {
        Ok(Events(self.0.events()?, self.1.clone(), true))
    }
    fn reply_channel(&self, capacity: NonZeroUsize) -> (fake::Sink, fake::Source) {
        self.0.reply_channel(capacity)
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
        source: fake::Source,
        deadline: Duration,
    ) -> Result<PlayOutcome, echo_link::Error> {
        self.0.play_with_timeout(format, source, deadline).await
    }
    async fn cut_with_stats(&self) -> Result<CutStats, echo_link::Error> {
        let result = self.0.cut_with_stats().await;
        self.1.stopped.store(true, Ordering::SeqCst);
        self.1.observer.finish(Ok(()));
        result
    }
    async fn fail_run(&self, code: RunFailure) -> Result<(), echo_link::Error> {
        self.0.fail_run(code).await
    }
    async fn pause(&self) -> Result<(), echo_link::Error> {
        self.0.pause().await
    }
    async fn close(&self) -> Result<(), echo_link::Error> {
        self.0.close().await
    }
}

pub(crate) fn settings(directory: &Path, live: &str) -> echo_bridge::bridge_input::Settings {
    let mut settings = echo_bridge::bridge_input::parse(
        &[],
        [
            Some("127.0.0.1:1".into()),
            Some(live.into()),
            Some(directory.join("trace").display().to_string()),
        ],
    )
    .unwrap()
    .unwrap();
    settings.lab = false;
    settings
}

pub(crate) async fn progress(counter: &AtomicUsize, previous: usize) -> usize {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let count = counter.load(Ordering::SeqCst);
            if count > previous {
                return count;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "T3 production runtime and local Live sockets"]
async fn bridge_refusal_continuously_ready_input_delivers_pre_pcm_error() {
    let directory = tempfile::tempdir().unwrap();
    let live = bridge_peer::Live::bind_frames(0).await;
    let input = Input::default();
    let (stop, shutdown) = tokio::sync::oneshot::channel();
    let runtime = tokio::spawn(echo_bridge::bridge_runtime::execute_controlled(
        settings(directory.path(), &live.url),
        echo_bridge::bridge_runtime::Control {
            utc_day: Arc::new(|| 0),
            shutdown,
        },
        input.clone(),
    ));
    input.wake();
    progress(&live.microphone_frames, 0).await;
    let before = input.frames.load(Ordering::SeqCst);
    live.send_acknowledged(json!({"type":"error"}).to_string()).await;
    bridge_wire_auth_failure::wait_event(&directory.path().join("trace"), "run.failed").await;
    let commands = input.observer.commands();
    assert_eq!(
        commands
            .iter()
            .filter(|c| matches!(c, fake::Command::FailRun(RunFailure::Unavailable)))
            .count(),
        1
    );
    assert_eq!(
        commands
            .iter()
            .filter(|c| matches!(c, fake::Command::Frame(_) | fake::Command::Close))
            .count(),
        0
    );
    assert_eq!(
        commands.iter().filter(|c| matches!(c, fake::Command::Connect)).count(),
        1
    );
    assert!(input.frames.load(Ordering::SeqCst) >= before);
    assert!(
        !trace(&directory.path().join("trace"))
            .iter()
            .any(|e| e["event"] == "mic.frames" && e["phase"] == "completed")
    );
    writeln!(
        std::io::stdout(),
        "continuous refusal: frames_before={before} frames_at_cut={} errors=1 pcm=0 disconnects=0",
        input.frames.load(Ordering::SeqCst)
    )
    .unwrap();
    stop.send(true).unwrap();
    assert_eq!(runtime.await.unwrap(), Ok(()));
}
