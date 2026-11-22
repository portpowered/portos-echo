//! T1: the listen window after a speech start that expires, on the real run
//! loop with the Echo's own windows: a 15 s listen window, a 60 s no-answer
//! window and the default 20 s speech-start expiry. Memory-only ports, paused
//! time. The device run of 2026-11-21: a reply played, the room was heard
//! once, the pinned V3 session sent no transcript and no speech stop, and the
//! run stayed open until the user said "end the session".
use super::*;
use crate::bridge_voice::Stats;
use crate::bridge_voice::voice_wait::{Mark, run_marks::SPEECH_START_EXPIRY};

const LISTEN: Duration = Duration::from_secs(15);
const NO_ANSWER: Duration = Duration::from_secs(60);

fn echo_windows() -> Options {
    Options {
        listen_timeout: Some(LISTEN),
        session_max: Duration::from_secs(600),
        reply_lead_ms: 200,
        response_deadline: Instant::now() + NO_ANSWER,
        played_timeout: Duration::from_secs(1),
        muted: false,
    }
}

/// What the run reports: played replies and its structure-only marks.
#[derive(Default)]
struct Seen {
    played: Cell<usize>,
    marks: RefCell<Vec<Mark>>,
}

impl Seen {
    fn record(&self, notice: &Notice) {
        match notice {
            Notice::ReplyPlayed => self.played.set(self.played.get() + 1),
            Notice::Mark(mark) => self.marks.borrow_mut().push(*mark),
            _ => (),
        }
    }

    /// Each mark as `event:reason` or `event:window`, in order.
    fn events(&self) -> Vec<String> {
        self.marks.borrow().iter().map(label).collect()
    }
}

fn label(mark: &Mark) -> String {
    let fields = mark.fields();
    let why = fields["reason"].as_str().or(fields["window"].as_str());
    why.map_or_else(|| mark.event().to_owned(), |why| format!("{}:{why}", mark.event()))
}

fn end_runs(observer: &Observer) -> usize {
    observer
        .commands()
        .iter()
        .filter(|command| **command == Command::EndRun)
        .count()
}

/// Run one reply to its Played receipt; returns the run and when it settled.
async fn replied<'a>(
    client: &'a echo_link::fake::FakeLink,
    events: &'a mut <echo_link::fake::FakeLink as EchoLink>::Events,
    port: &'a mut Port,
    tx: &mpsc::Sender<MediaEvent>,
    observer: &Observer,
    seen: &'a Seen,
) -> (Pin<Box<impl Future<Output = Result<Stats, Error>> + 'a>>, Instant) {
    observer.wake("Computer", vec![]);
    let Some(LinkEvent::Wake(microphone)) = events.recv().await.unwrap() else {
        unreachable!()
    };
    let mut work = Box::pin(crate::bridge_voice::run(
        client,
        events,
        microphone,
        port,
        echo_windows(),
        std::future::pending(),
        |notice| {
            seen.record(&notice);
            Ok(())
        },
    ));
    reply(tx, 5000).await;
    settle(&mut work, observer, &seen.played, 1).await;
    (work, Instant::now())
}

/// Hold the run to one millisecond before `bound`, then past it: it is still
/// open just before, and once the speaker has played out it ends. Returns the
/// run's result and when it ended.
async fn ends_at<W: Future<Output = Result<Stats, Error>>>(
    mut work: Pin<Box<W>>,
    observer: &Observer,
    bound: Instant,
) -> (Result<Stats, Error>, Instant) {
    let early = bound.checked_duration_since(Instant::now()).unwrap();
    tokio::time::advance(early.checked_sub(Duration::from_millis(1)).unwrap()).await;
    poll(work.as_mut()).await;
    assert_eq!(end_runs(observer), 0, "the run ended before its window");
    tokio::time::advance(Duration::from_millis(1)).await;
    for _ in 0..50 {
        poll(work.as_mut()).await;
        if observer.commands().last() == Some(&Command::Stop) {
            break;
        }
        tokio::time::advance(Duration::from_millis(20)).await;
    }
    assert_eq!(
        observer.commands().last(),
        Some(&Command::Stop),
        "the run is still listening {:?} past its window",
        Instant::now() - bound
    );
    observer.finish(Ok(()));
    let ended = work.await;
    (ended, Instant::now())
}

/// The device run: a speech start 4 s into the listen window that is never
/// transcribed and never stopped. The latch expires and the listen window
/// resumes from that expiry: the run ends 15 s later, not one no-answer window
/// later, and says why.
#[tokio::test(start_paused = true)]
async fn voice_listen_resumes_its_window_after_a_speech_start_expires() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    let (tx, rx) = mpsc::channel(64);
    let mut port = Port(rx, std::rc::Rc::default());
    let seen = Seen::default();
    let (mut work, settled) = replied(&client, &mut events, &mut port, &tx, &observer, &seen).await;
    tokio::time::advance(Duration::from_secs(4)).await;
    tx.send(MediaEvent::Control(Event::SpeechStarted)).await.unwrap();
    poll(work.as_mut()).await;
    let expired = Instant::now() + SPEECH_START_EXPIRY;
    tokio::time::advance(SPEECH_START_EXPIRY).await;
    poll(work.as_mut()).await;
    assert_eq!(seen.events(), ["speech.latched:expired"]);
    let (result, ended) = ends_at(work, &observer, expired + LISTEN).await;
    let stats = result.unwrap();
    assert!(
        ended - expired < LISTEN + Duration::from_secs(1),
        "ended {:?} after the expiry",
        ended - expired
    );
    assert!(ended - settled < Duration::from_secs(4) + SPEECH_START_EXPIRY + LISTEN + Duration::from_secs(1));
    assert_eq!((stats.reply_received, seen.played.get()), (12, 1));
    assert_eq!(end_runs(&observer), 1);
    assert_eq!(seen.events(), ["speech.latched:expired", "run.lapsed:listen"]);
}

/// The plain case: nothing is heard after the reply, and the run ends exactly
/// one listen window after it settles, with no lapse mark.
#[tokio::test(start_paused = true)]
async fn voice_listen_silent_after_a_reply_ends_at_its_window() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    let (tx, rx) = mpsc::channel(64);
    let mut port = Port(rx, std::rc::Rc::default());
    let seen = Seen::default();
    let (mut work, settled) = replied(&client, &mut events, &mut port, &tx, &observer, &seen).await;
    tokio::time::advance(LISTEN.checked_sub(Duration::from_millis(1)).unwrap()).await;
    poll(work.as_mut()).await;
    assert_eq!(end_runs(&observer), 0);
    tokio::time::advance(Duration::from_millis(1)).await;
    let stats = work.await.unwrap();
    assert_eq!(Instant::now() - settled, LISTEN);
    assert_eq!((stats.reply_received, seen.played.get()), (12, 1));
    assert_eq!(end_runs(&observer), 1);
    assert_eq!(seen.events(), Vec::<String>::new());
}

/// A hand-off that starts before the latch expires still owes its answer: the
/// resumed listen window never closes the run over delegated work.
#[tokio::test(start_paused = true)]
async fn voice_listen_expired_speech_start_keeps_a_running_hand_off() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    let (tx, rx) = mpsc::channel(64);
    let mut port = Port(rx, std::rc::Rc::default());
    let seen = Seen::default();
    let (mut work, _) = replied(&client, &mut events, &mut port, &tx, &observer, &seen).await;
    tokio::time::advance(Duration::from_secs(4)).await;
    tx.send(MediaEvent::Control(Event::SpeechStarted)).await.unwrap();
    tx.send(MediaEvent::Control(handoff(true))).await.unwrap();
    poll(work.as_mut()).await;
    tokio::time::advance(SPEECH_START_EXPIRY).await;
    poll(work.as_mut()).await;
    assert_eq!(seen.events(), ["speech.latched:expired"]);
    // Twice the listen window past the expiry, the hand-off is still running.
    tokio::time::advance(LISTEN * 2).await;
    poll(work.as_mut()).await;
    assert_eq!(end_runs(&observer), 0);
    tx.send(MediaEvent::Control(handoff(false))).await.unwrap();
    reply(&tx, -5000).await;
    settle(&mut work, &observer, &seen.played, 2).await;
    let settled = Instant::now();
    tokio::time::advance(LISTEN).await;
    let stats = work.await.unwrap();
    assert_eq!(Instant::now() - settled, LISTEN);
    assert_eq!((stats.reply_received, seen.played.get()), (24, 2));
    assert_eq!(end_runs(&observer), 1);
}

fn handoff(started: bool) -> Event {
    let (thread, turn) = ("thread-1".to_owned(), "turn-1".to_owned());
    if started {
        Event::TurnStarted { thread, turn }
    } else {
        Event::TurnCompleted {
            thread,
            turn,
            answered: true,
        }
    }
}
