//! T1: one run owner, memory-only PCM/control and device boundaries, paused time.
use super::*;
use echo_link::{
    LinkConnector, MicFrame,
    fake::{Command, FakeConnector, Observer},
};
use std::{
    collections::VecDeque,
    future::poll_fn,
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
};
use tokio::sync::mpsc;

struct Port {
    events: mpsc::Receiver<Result<MediaEvent, Error>>,
    ready: VecDeque<PcmFrame>,
    sent: Arc<Mutex<Vec<PcmFrame>>>,
    reject: Option<usize>,
}
impl VoicePort for Port {
    fn send(&self, frame: PcmFrame) -> Result<(), Error> {
        let mut sent = self.sent.lock().unwrap();
        if self.reject == Some(sent.len()) {
            return Err(Error::Unavailable);
        }
        sent.push(frame);
        Ok(())
    }
    async fn next(&mut self) -> Result<MediaEvent, Error> {
        self.events.recv().await.unwrap_or(Err(Error::Unavailable))
    }
    fn ready_audio(&mut self) -> impl Future<Output = Result<Option<PcmFrame>, Error>> {
        std::future::ready(Ok(self.ready.pop_front()))
    }
}
fn port() -> (Port, mpsc::Sender<Result<MediaEvent, Error>>) {
    let (tx, events) = mpsc::channel(32);
    (
        Port {
            events,
            ready: VecDeque::new(),
            sent: Arc::default(),
            reject: None,
        },
        tx,
    )
}
fn options() -> Options {
    Options {
        reply_lead_ms: 200,
        response_deadline: Instant::now() + Duration::from_secs(10),
        played_timeout: Duration::from_secs(1),
        muted: false,
    }
}
async fn pending<F: Future>(mut future: Pin<&mut F>) {
    for _ in 0..1024 {
        poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }
}
fn frames(observer: &Observer) -> Vec<Vec<u8>> {
    observer
        .commands()
        .into_iter()
        .filter_map(|command| match command {
            Command::Frame(bytes) => Some(bytes),
            _ => None,
        })
        .collect()
}
fn wake(observer: &Observer, samples: usize) {
    observer.wake(
        "Computer",
        vec![MicFrame {
            format: crate::bridge_audio::format(16_000),
            payload: vec![0; samples * 2],
        }],
    );
}
async fn microphone<E: LinkEvents>(events: &mut E) -> E::Run {
    match events.recv().await.unwrap().unwrap() {
        LinkEvent::Wake(run) => run,
        _ => unreachable!(),
    }
}
fn done() -> MediaEvent {
    MediaEvent::Control(Event::ReplyDone {
        text: "fixture reply".into(),
    })
}

#[path = "voice_cases.rs"]
mod cases;

#[tokio::test(start_paused = true)]
async fn voice_transcript_drains_accepted_pcm_paces_then_requires_actual_played() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 640);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    port.ready = (0..32).map(|_| [0; 320]).collect();
    tx.send(Ok(done())).await.unwrap();
    let sent = port.sent.clone();
    let mut notices = Vec::new();
    let mut work = Box::pin(run(
        &client,
        &mut events,
        mic,
        &mut port,
        options(),
        std::future::pending(),
        |notice| {
            notices.push(match notice {
                Notice::ReplyStarted => "speaking",
                Notice::ReplyPlayed => "played",
                Notice::Transcript { .. } => "transcript",
                Notice::Device(_) => "device",
            });
            Ok(())
        },
    ));
    pending(work.as_mut()).await;
    assert_eq!(frames(&observer), vec![vec![0; 960]; 10]);
    assert_eq!(sent.lock().unwrap().len(), 2); // identity-rate capture has no filter delay
    for count in 11..=32 {
        tokio::time::advance(Duration::from_millis(20)).await;
        pending(work.as_mut()).await;
        assert_eq!(frames(&observer).len(), count);
    }
    assert_eq!(observer.commands().last(), Some(&Command::Stop));
    assert_eq!(sent.lock().unwrap().len(), 2);
    // Fully drained and audio-stop sent, but no played receipt: still pending.
    pending(work.as_mut()).await;
    observer.finish(Ok(()));
    assert_eq!(
        work.await.unwrap(),
        Stats {
            received_chunks: 1,
            dropped_chunks: 0,
            microphone_chunks: 1,
            microphone_frames: 2,
            microphone_tail: 0,
            reply_received: 32,
            reply_sent: 32,
            reply_dropped: 0
        }
    );
    assert_eq!(notices, vec!["transcript", "speaking", "device", "played"]);
}

#[tokio::test(start_paused = true)]
async fn voice_empty_reply_still_requires_stop_and_played() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 1);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    tx.send(Ok(done())).await.unwrap();
    let mut work = Box::pin(run(
        &client,
        &mut events,
        mic,
        &mut port,
        options(),
        std::future::pending(),
        |_| Ok(()),
    ));
    pending(work.as_mut()).await;
    assert_eq!(frames(&observer), Vec::<Vec<u8>>::new());
    assert_eq!(observer.commands().last(), Some(&Command::Stop));
    observer.finish(Ok(()));
    let stats = work.await.unwrap();
    assert_eq!(stats.microphone_frames, 0);
    assert_eq!(stats.microphone_tail, 1);
    assert_eq!(stats.reply_sent, 0);
}

#[tokio::test(start_paused = true)]
async fn voice_user_done_and_legacy_other_never_complete_a_reply() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    tx.send(Ok(MediaEvent::Control(Event::Transcript {
        role: "user".into(),
        text: "question".into(),
        done: true,
    })))
    .await
    .unwrap();
    tx.send(Ok(MediaEvent::Control(Event::Other))).await.unwrap();
    let end = options().response_deadline;
    let result = run(
        &client,
        &mut events,
        mic,
        &mut port,
        options(),
        std::future::pending(),
        |_| Ok(()),
    )
    .await;
    assert_eq!(result, Err(Error::Deadline));
    assert_eq!(Instant::now(), end);
    assert!(observer.commands().contains(&Command::Cut(0)));
    assert!(!observer.commands().contains(&Command::Stop));
}

#[tokio::test(start_paused = true)]
async fn voice_missing_played_hits_its_own_deadline() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    tx.send(Ok(done())).await.unwrap();
    let start = Instant::now();
    assert_eq!(
        run(
            &client,
            &mut events,
            mic,
            &mut port,
            options(),
            std::future::pending(),
            |_| Ok(())
        )
        .await,
        Err(Error::Deadline)
    );
    assert_eq!(Instant::now() - start, Duration::from_secs(1));
    assert!(observer.commands().contains(&Command::Stop));
    assert!(observer.commands().contains(&Command::Cut(0)));
}

#[tokio::test(start_paused = true)]
async fn voice_eof_cancellation_and_callback_failure_cannot_publish_played() {
    for failure in [Error::Unavailable, Error::Control, Error::Screen] {
        let observer = Observer::default();
        let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = client.events().unwrap();
        wake(&observer, 0);
        let mic = microphone(&mut events).await;
        let (mut port, tx) = port();
        if failure == Error::Unavailable {
            drop(tx);
        } else {
            tx.send(Ok(done())).await.unwrap();
        }
        let mut played = 0;
        let result = run(
            &client,
            &mut events,
            mic,
            &mut port,
            options(),
            async {
                if failure != Error::Control {
                    std::future::pending::<()>().await;
                }
            },
            |notice| {
                if matches!(notice, Notice::ReplyPlayed) {
                    played += 1;
                }
                if failure == Error::Screen {
                    Err(Error::Screen)
                } else {
                    Ok(())
                }
            },
        )
        .await;
        assert_eq!(result, Err(failure));
        assert_eq!(played, 0);
        assert!(observer.commands().contains(&Command::Cut(0)));
    }
}

#[tokio::test(start_paused = true)]
async fn voice_late_media_is_an_error_instead_of_silent_reply_truncation() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    tx.send(Ok(done())).await.unwrap();
    let mut work = Box::pin(run(
        &client,
        &mut events,
        mic,
        &mut port,
        options(),
        std::future::pending(),
        |_| Ok(()),
    ));
    pending(work.as_mut()).await;
    tx.send(Ok(MediaEvent::Audio(Box::new([123; 320])))).await.unwrap();
    assert_eq!(work.await, Err(Error::Audio));
    assert_eq!(frames(&observer).len(), 0);
    assert!(observer.commands().contains(&Command::Cut(0)));
}
