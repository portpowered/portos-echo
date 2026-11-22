//! T1: after a barge-in cut, nothing the user says holds the run past its cap,
//! and a speech start with no transcript text stops holding it at all after
//! the speech-start expiry. The pinned V3 session forwards no speech-stopped.
//! Memory-only ports, paused time, the Echo's own windows.
use super::*;
use crate::bridge_runtime::bridge_pacing::BARGE_QUIET;
use std::cell::RefCell;
use voice_wait::{Mark, run_marks::SPEECH_START_EXPIRY};

/// `voice.session_max_seconds` on the Echo.
const CAP: Duration = Duration::from_secs(600);
/// The Echo's no-answer window.
const WINDOW: Duration = Duration::from_secs(60);

fn echo_windows() -> Options {
    Options {
        listen_timeout: Some(Duration::from_secs(15)),
        session_max: CAP,
        reply_lead_ms: 200,
        response_deadline: Instant::now() + WINDOW,
        played_timeout: Duration::from_secs(5),
        muted: false,
    }
}

fn user(text: &str, done: bool) -> MediaEvent {
    MediaEvent::Control(Event::Transcript {
        role: "user".into(),
        text: text.into(),
        done,
    })
}

fn speech_started() -> MediaEvent {
    MediaEvent::Control(Event::SpeechStarted)
}

/// The incident's tail: the barge's words, a speech start the session never
/// transcribes, and the model's acknowledgement, begun and never finished.
fn acknowledged() -> Vec<MediaEvent> {
    vec![
        user("Computer, hold on for two minutes.", true),
        speech_started(),
        said("Sure"),
    ]
}

/// Poll the run without asserting that it is still pending.
async fn settle<F: Future>(mut work: Pin<&mut F>) -> Option<F::Output> {
    for _ in 0..1024 {
        let ready = poll_fn(|cx| Poll::Ready(work.as_mut().poll(cx))).await;
        if let Poll::Ready(output) = ready {
            return Some(output);
        }
    }
    None
}

type Events = mpsc::Sender<Result<MediaEvent, Error>>;
type Chatter = Option<fn() -> Vec<MediaEvent>>;

/// How the run ended, when, and the trace marks it reported.
struct Ended {
    result: Result<Stats, Error>,
    at: Duration,
    marks: Vec<Mark>,
}

impl Ended {
    fn latched(&self, wanted: &str) -> Vec<(Duration, bool)> {
        let latched = self.marks.iter().filter_map(|mark| match *mark {
            Mark::Latched {
                held,
                reason,
                transcribed,
            } => (reason == wanted).then_some((held, transcribed)),
            _ => None,
        });
        latched.collect()
    }

    fn capped(&self) -> Vec<&'static str> {
        let capped = self.marks.iter().filter_map(|mark| match *mark {
            Mark::Capped(during) => Some(during),
            _ => None,
        });
        capped.collect()
    }
}

/// The time question answered, then cut by "Computer, hold on for two
/// minutes." `after` is what the call delivers once the cut has settled;
/// `chatter` is heard every ten seconds from then on. None while the run is
/// still open 900 s in.
async fn barge_then(after: Vec<MediaEvent>, chatter: Chatter) -> Option<Ended> {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    let start = Instant::now();
    let marks = RefCell::new(Vec::new());
    let mut work = Box::pin(run(
        &client,
        &mut events,
        mic,
        &mut port,
        echo_windows(),
        std::future::pending(),
        |notice| {
            if let Notice::Mark(mark) = notice {
                marks.borrow_mut().push(mark);
            }
            Ok(())
        },
    ));
    answer_then_barge(work.as_mut(), &tx, &observer).await;
    send(&tx, after).await;
    let result = wait_out(work.as_mut(), &tx, chatter).await?;
    let at = start.elapsed();
    drop(work);
    Some(Ended {
        result,
        at,
        marks: marks.take(),
    })
}

/// The answer plays, a speech start barges in, the cut settles.
async fn answer_then_barge<F: Future>(mut work: Pin<&mut F>, tx: &Events, observer: &Observer) {
    send(tx, [user("What time is it?", true), said("It's three")]).await;
    for _ in 0..30 {
        send(tx, audible(1)).await;
        pending(work.as_mut()).await;
        tokio::time::advance(Duration::from_millis(20)).await;
    }
    send(tx, [speech_started()]).await;
    pending(work.as_mut()).await;
    tokio::time::advance(BARGE_QUIET).await;
    pending(work.as_mut()).await;
    let cuts = observer
        .commands()
        .iter()
        .filter(|c| matches!(c, Command::Cut(_)))
        .count();
    assert_eq!(cuts, 1, "the barge-in cut the answer");
    observer.finish(Ok(()));
    pending(work.as_mut()).await;
}

/// Step paused time a second at a time for 900 s, or until the run ends.
async fn wait_out<F: Future>(mut work: Pin<&mut F>, tx: &Events, chatter: Chatter) -> Option<F::Output> {
    for second in 0..900 {
        if let Some(chatter) = chatter.filter(|_| second % 10 == 0) {
            send(tx, chatter()).await;
        }
        if let Some(result) = settle(work.as_mut()).await {
            return Some(result);
        }
        tokio::time::advance(Duration::from_secs(1)).await;
    }
    None
}

/// The incident with nothing more from anyone: the silent speech start
/// expires, the user counts as quiet again, and the acknowledgement that
/// began and stalled fails as no answer one no-answer window later. Before
/// the expiry it held the reply open to the 600 s run cap.
#[tokio::test(start_paused = true)]
async fn barge_then_silent_speech_start_expires_and_the_run_ends_unanswered() {
    let ended = barge_then(acknowledged(), None).await;
    assert!(ended.is_some(), "the run is still open 900 s after the wake");
    let ended = ended.unwrap();
    assert_eq!(ended.result.as_ref().map(|_| ()), Err(&Error::NoResponse));
    let by = SPEECH_START_EXPIRY + WINDOW + Duration::from_secs(3);
    assert!(
        ended.at > SPEECH_START_EXPIRY && ended.at <= by,
        "ended at {:?}",
        ended.at
    );
    let expired = ended.latched("expired");
    assert_eq!(expired.len(), 1, "one speech.latched mark, reason expired");
    let (held, transcribed) = expired[0];
    assert!(held >= SPEECH_START_EXPIRY && held < SPEECH_START_EXPIRY + Duration::from_secs(1));
    assert!(!transcribed);
    assert_eq!(ended.capped(), Vec::<&str>::new());
}

/// Transcript text that keeps arriving keeps the user speaking, however long;
/// the reply it holds still ends one speech-start expiry past the run cap.
#[tokio::test(start_paused = true)]
async fn barge_then_transcribed_speech_that_never_ends_holds_the_reply_to_the_run_cap() {
    let ended = barge_then(acknowledged(), Some(|| vec![user("and then", false)])).await;
    assert!(
        ended.is_some(),
        "the run is still open 900 s after the wake, past the {CAP:?} run cap"
    );
    let ended = ended.unwrap();
    assert_eq!(ended.result.as_ref().map(|_| ()), Err(&Error::SessionCeiling));
    let cap = CAP + SPEECH_START_EXPIRY;
    assert!(
        ended.at > CAP && ended.at <= cap + Duration::from_secs(1),
        "ended at {:?}",
        ended.at
    );
    assert_eq!(ended.capped(), ["reply"]);
    let open = ended.latched("run_end");
    assert_eq!(open.len(), 1, "the latch still open at the cap is reported");
    assert!(open[0].1, "partial transcripts renewed it");
    assert_eq!(ended.latched("expired"), Vec::new());
}

/// After the cut the run listens for the model's answer, and speech renews
/// that listen window. Speech that never stops still ends at the run cap.
#[tokio::test(start_paused = true)]
async fn barge_then_endless_room_speech_ends_the_listening_run_at_its_cap() {
    let after = vec![user("Computer, hold on for two minutes.", true)];
    let ended = barge_then(after, Some(|| vec![speech_started(), user("Talk in the room.", true)])).await;
    assert!(
        ended.is_some(),
        "the run is still open 900 s after the wake, past the {CAP:?} run cap"
    );
    let ended = ended.unwrap();
    assert!(ended.result.is_ok(), "a capped listen closes like an idle one");
    assert!(ended.at <= CAP + Duration::from_secs(1), "ended at {:?}", ended.at);
    assert_eq!(ended.capped(), ["listen"]);
    assert!(
        !ended.latched("cleared").is_empty(),
        "each whole transcript clears its latch"
    );
}
