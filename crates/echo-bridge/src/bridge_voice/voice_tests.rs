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
    retired: usize,
}
impl VoicePort for Port {
    fn retire_audio(&mut self) {
        self.retired += 1;
    }
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
            retired: 0,
        },
        tx,
    )
}
fn options() -> Options {
    Options {
        listen_timeout: None,
        session_max: Duration::from_secs(600),
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

#[path = "voice_barge_cases.rs"]
mod barge_cases;
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
                Notice::EarconPlayed(_) | Notice::Thinking => "indication",
                Notice::ReplyStarted => "speaking",
                Notice::ReplyPaused => "paused",
                Notice::ReplyPlayed | Notice::ReplySettled(_) => "played",
                Notice::Transcript { .. } => "transcript",
                Notice::Device(_) => "device",
                Notice::SessionTool(_) => "session_tool",
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
    let stats = work.await.unwrap();
    assert_eq!(
        stats,
        Stats {
            received_chunks: 1,
            microphone_chunks: 1,
            microphone_frames: 2,
            reply_received: 32,
            reply_sent: 32,
            quality: stats.quality,
            ..Stats::default()
        }
    );
    // Silence never opens the mouth.
    assert_eq!(notices, vec!["transcript", "device", "played"]);
}
#[tokio::test(start_paused = true)]
async fn voice_short_or_empty_reply_waits_for_played_without_early_mouth_open() {
    for count in [0, 1] {
        let observer = Observer::default();
        let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = client.events().unwrap();
        wake(&observer, 1);
        let mic = microphone(&mut events).await;
        let (mut port, tx) = port();
        port.ready = (0..count).map(|_| [123; 320]).collect();
        tx.send(Ok(done())).await.unwrap();
        let start = Instant::now();
        let transitions = std::cell::RefCell::new(Vec::new());
        let mut work = Box::pin(run(
            &client,
            &mut events,
            mic,
            &mut port,
            options(),
            std::future::pending(),
            |notice| {
                let open = match notice {
                    Notice::ReplyStarted => Some(true),
                    Notice::ReplyPaused => Some(false),
                    _ => None,
                };
                if let Some(open) = open {
                    transitions.borrow_mut().push((Instant::now() - start, open));
                }
                Ok(())
            },
        ));
        pending(work.as_mut()).await;
        assert_eq!(frames(&observer).len(), count);
        assert_eq!(observer.commands().last(), Some(&Command::Stop));
        assert!(transitions.borrow().is_empty());
        observer.finish(Ok(())); // Deliberately earlier than the output delay.
        if count == 1 {
            pending(work.as_mut()).await;
            assert!(transitions.borrow().is_empty());
        }
        let stats = work.await.unwrap();
        assert_eq!((stats.microphone_frames, stats.microphone_tail), (0, 1));
        assert_eq!(stats.reply_sent, u64::try_from(count).unwrap());
        let expected = if count == 0 {
            vec![]
        } else {
            vec![(Duration::from_millis(200), true), (Duration::from_millis(220), false)]
        };
        assert_eq!(*transitions.borrow(), expected);
    }
}
#[tokio::test(start_paused = true)]
async fn voice_user_done_and_legacy_other_never_complete_a_reply() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    tx.send(Ok(MediaEvent::Audio(Box::new([456; 320])))).await.unwrap();
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
    assert_eq!(result, Err(Error::NoResponse));
    assert_eq!(port.retired, 0);
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
        assert_eq!(
            result,
            Err(if failure == Error::Control {
                Error::Deadline
            } else {
                failure
            })
        );
        assert_eq!(played, 0);
        assert!(observer.commands().contains(&Command::Cut(0)));
    }
}
#[tokio::test(start_paused = true)]
async fn voice_all_webrtc_media_after_done_is_discarded() {
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
    for frame in [[0; 320], [64; 320], [-64; 320]] {
        tx.send(Ok(MediaEvent::Audio(Box::new(frame)))).await.unwrap();
        pending(work.as_mut()).await;
    }
    assert_eq!(frames(&observer).len(), 0);
    tx.send(Ok(MediaEvent::Audio(Box::new([65; 320])))).await.unwrap();
    pending(work.as_mut()).await;
    observer.finish(Ok(()));
    assert_eq!(work.await.unwrap().reply_received, 4);
    assert_eq!(frames(&observer).len(), 0);
}

fn turn(started: bool) -> MediaEvent {
    let (thread, turn, answered) = ("thread-1".to_owned(), "turn-1".to_owned(), true);
    MediaEvent::Control(if started {
        Event::TurnStarted { thread, turn }
    } else {
        Event::TurnCompleted { thread, turn, answered }
    })
}

fn said(text: &str) -> MediaEvent {
    MediaEvent::Control(Event::Transcript {
        role: "assistant".into(),
        text: text.into(),
        done: false,
    })
}

async fn send(tx: &mpsc::Sender<Result<MediaEvent, Error>>, events: impl IntoIterator<Item = MediaEvent>) {
    for event in events {
        tx.send(Ok(event)).await.unwrap();
    }
}

fn audible(count: usize) -> Vec<MediaEvent> {
    (0..count).map(|_| MediaEvent::Audio(Box::new([123; 320]))).collect()
}

async fn tick<F: Future>(mut work: Pin<&mut F>, frames: usize) {
    for _ in 0..frames {
        tokio::time::advance(Duration::from_millis(20)).await;
        pending(work.as_mut()).await;
    }
}

/// Live calls showed both orders: the filler's done before turn/completed, and
/// turn/completed while the filler was still being spoken.
async fn delegated_answer(filler: Vec<MediaEvent>, answer: MediaEvent) -> (u64, u64) {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    let mut work = Box::pin(run(
        &client,
        &mut events,
        mic,
        &mut port,
        options(),
        std::future::pending(),
        |_| Ok(()),
    ));
    send(&tx, filler).await;
    pending(work.as_mut()).await;
    send(&tx, audible(12)).await;
    pending(work.as_mut()).await;
    assert_eq!(frames(&observer).len(), 10);
    assert!(!observer.commands().contains(&Command::EndRun));
    send(&tx, [answer, done()]).await;
    tick(work.as_mut(), 2).await;
    assert_eq!(frames(&observer).len(), 12);
    assert_eq!(observer.commands().last(), Some(&Command::Stop));
    observer.finish(Ok(()));
    let stats = work.await.unwrap();
    assert_eq!(observer.commands().iter().filter(|c| **c == Command::EndRun).count(), 1);
    (stats.reply_received, stats.reply_sent)
}
#[tokio::test(start_paused = true)]
async fn voice_filler_done_before_the_delegated_turn_completes_keeps_the_reply_open() {
    let filler = vec![turn(true), said("One sec."), done(), turn(false)];
    assert_eq!(delegated_answer(filler, said("It's three.")).await, (12, 12));
}
#[tokio::test(start_paused = true)]
async fn voice_filler_done_after_the_delegated_turn_completes_keeps_the_reply_open() {
    let filler = vec![turn(true), said("One sec."), turn(false), done()];
    assert_eq!(delegated_answer(filler, said("It's three.")).await, (12, 12));
}
#[tokio::test(start_paused = true)]
async fn voice_answer_begun_before_turn_completed_still_ends_the_reply() {
    let filler = vec![turn(true), said("One sec."), done(), said("It's")];
    assert_eq!(delegated_answer(filler, turn(false)).await, (12, 12));
}
#[tokio::test(start_paused = true)]
async fn voice_filler_running_on_into_the_answer_ends_the_reply_at_its_done() {
    let filler = vec![turn(true), said("One sec."), turn(false)];
    assert_eq!(delegated_answer(filler, said("It's three.")).await, (12, 12));
}
#[tokio::test(start_paused = true)]
async fn voice_utterance_in_progress_when_the_turn_starts_is_the_filler() {
    let filler = vec![said("One sec."), turn(true), done(), turn(false)];
    assert_eq!(delegated_answer(filler, said("It's three.")).await, (12, 12));
}
#[tokio::test(start_paused = true)]
async fn voice_delegated_answer_missing_the_deadline_ends_after_the_filler_without_failing() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    send(&tx, [turn(true), said("One sec."), done()]).await;
    send(&tx, audible(12)).await;
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
    tokio::time::advance(Duration::from_secs(9)).await;
    pending(work.as_mut()).await;
    // The filler played up to the resampler's held tail; the reply stays open.
    assert_eq!(frames(&observer).len(), 11);
    assert_ne!(observer.commands().last(), Some(&Command::Stop));
    assert!(!observer.commands().contains(&Command::EndRun));
    tokio::time::advance(Duration::from_secs(1)).await;
    pending(work.as_mut()).await;
    tick(work.as_mut(), 1).await;
    // The deadline flushes the tail and ends the reply instead of failing.
    assert_eq!(frames(&observer).len(), 12);
    assert_eq!(observer.commands().last(), Some(&Command::Stop));
    // The abandoned answer arriving late is dropped, not a fatal late-media error.
    send(&tx, audible(1)).await;
    pending(work.as_mut()).await;
    observer.finish(Ok(()));
    let stats = work.await.unwrap();
    assert_eq!((stats.reply_received, stats.reply_sent), (13, 12));
    assert_eq!(observer.commands().last(), Some(&Command::EndRun));
}

fn assert_drained_reply(observer: &Observer, old_frames: usize) {
    if old_frames == 3 {
        assert_eq!(observer.commands().last(), Some(&Command::Stop));
    }
}
