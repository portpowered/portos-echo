//! T1: after a barge-in cut, nothing the user says holds the run past its cap.
//! The pinned V3 session forwards no speech-stopped, so a speech start the
//! session never transcribes leaves the bridge believing the user still
//! speaks. Memory-only ports, paused time, the Echo's own windows.
use super::*;
use crate::bridge_runtime::bridge_pacing::BARGE_QUIET;

/// `voice.session_max_seconds` on the Echo.
const CAP: Duration = Duration::from_secs(600);

fn echo_windows() -> Options {
    Options {
        listen_timeout: Some(Duration::from_secs(15)),
        session_max: CAP,
        reply_lead_ms: 200,
        response_deadline: Instant::now() + Duration::from_secs(60),
        played_timeout: Duration::from_secs(5),
        muted: false,
    }
}

fn user(text: &str) -> MediaEvent {
    MediaEvent::Control(Event::Transcript {
        role: "user".into(),
        text: text.into(),
        done: true,
    })
}

fn speech_started() -> MediaEvent {
    MediaEvent::Control(Event::SpeechStarted)
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

/// The time question answered, then cut by "Computer, hold on for two
/// minutes." `after` is what the call delivers once the cut has settled;
/// `chatter` is speech heard every ten seconds from then on. Returns the run's
/// result and when it ended, or None while it is still open 900 s in.
async fn barge_then(after: Vec<MediaEvent>, chatter: bool) -> Option<(Result<Stats, Error>, Duration)> {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    let start = Instant::now();
    let mut work = Box::pin(run(
        &client,
        &mut events,
        mic,
        &mut port,
        echo_windows(),
        std::future::pending(),
        |_| Ok(()),
    ));
    answer_then_barge(work.as_mut(), &tx, &observer).await;
    send(&tx, after).await;
    wait_out(work.as_mut(), &tx, chatter)
        .await
        .map(|result| (result, start.elapsed()))
}

/// The answer plays, a speech start barges in, the cut settles.
async fn answer_then_barge<F: Future>(mut work: Pin<&mut F>, tx: &Events, observer: &Observer) {
    send(tx, [user("What time is it?"), said("It's three")]).await;
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
async fn wait_out<F: Future>(mut work: Pin<&mut F>, tx: &Events, chatter: bool) -> Option<F::Output> {
    for second in 0..900 {
        if chatter && second % 10 == 0 {
            send(tx, [speech_started(), user("Talk in the room.")]).await;
        }
        if let Some(result) = settle(work.as_mut()).await {
            return Some(result);
        }
        tokio::time::advance(Duration::from_secs(1)).await;
    }
    None
}

/// The incident: the barge's own words are transcribed, then a speech start
/// the session never transcribes (V3 sends no speech-stopped), and the model's
/// acknowledgement begins and never finishes. The reply it opened waited on
/// the user's "speech" with no deadline at all, the run cap included.
#[tokio::test(start_paused = true)]
async fn barge_then_untranscribed_speech_start_and_unfinished_reply_end_at_the_run_cap() {
    let after = vec![
        user("Computer, hold on for two minutes."),
        speech_started(),
        said("Sure"),
    ];
    let outcome = barge_then(after, false).await;
    assert!(
        outcome.is_some(),
        "the run is still open 900 s after the wake, past the {CAP:?} run cap"
    );
    let (result, ended) = outcome.unwrap();
    assert_eq!(result.map(|_| ()), Err(Error::SessionCeiling));
    assert!(ended <= CAP + Duration::from_secs(1), "ended at {ended:?}");
}

/// After the cut the run listens for the model's answer, and speech renews
/// that listen window. Speech that never stops still ends at the run cap.
#[tokio::test(start_paused = true)]
async fn barge_then_endless_room_speech_ends_the_listening_run_at_its_cap() {
    let outcome = barge_then(vec![user("Computer, hold on for two minutes.")], true).await;
    assert!(
        outcome.is_some(),
        "the run is still open 900 s after the wake, past the {CAP:?} run cap"
    );
    let (result, ended) = outcome.unwrap();
    assert!(result.is_ok(), "a capped listen closes like an idle one");
    assert!(ended <= CAP + Duration::from_secs(1), "ended at {ended:?}");
}
