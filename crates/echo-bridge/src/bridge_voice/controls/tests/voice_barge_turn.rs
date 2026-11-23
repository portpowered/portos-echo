//! T1, one cut per user turn (owner, 2026-11-21): the first time the user
//! talks over the agent cuts it; for the rest of that user turn the agent's
//! backchannels ("mm-hm") play through; the cut re-arms once the turn ends.
//! A turn ends where the Live session says it does: the user's whole
//! transcript, its end of speech, the end of capture, or the speech-start
//! expiry. A stop and start of the session's speech detection is two turns.
//! The trace shows each decision as a structure-only `barge.*` mark.
use super::{yield_cases::stream, *};
use crate::bridge_voice::voice_wait::run_marks::MarkFields;
use serde_json::{Value, json};
use std::cell::RefCell;

const FRAME: Duration = Duration::from_millis(20);

type Rows = RefCell<Vec<(&'static str, Value)>>;
type Events = mpsc::Sender<Result<MediaEvent, Error>>;

fn quiet() -> usize {
    usize::try_from(BARGE_QUIET.as_millis() / FRAME.as_millis()).unwrap()
}

fn cuts(observer: &Observer) -> usize {
    command_count(observer, false)
}

/// Keep the run's `barge.*` trace marks, in order.
fn barge_rows(rows: &Rows) -> impl FnMut(Notice) -> Result<(), Error> + '_ {
    move |notice| {
        if let Notice::Mark(mark) = notice
            && mark.event().starts_with("barge.")
        {
            rows.borrow_mut().push((mark.event(), mark.fields()));
        }
        Ok(())
    }
}

fn speech(started: bool) -> MediaEvent {
    MediaEvent::control(if started {
        Event::SpeechStarted
    } else {
        Event::SpeechStopped
    })
}

fn trigger(signal: usize) -> &'static str {
    if signal == 0 {
        "speech_started"
    } else {
        "user_transcript"
    }
}

/// A reply sounds, `sign` talks over it, the model yields and the device
/// acknowledges the cut.
async fn cut_by<F: Future>(work: &mut Pin<Box<F>>, tx: &Events, observer: &Observer, sign: MediaEvent) {
    send(tx, [said("A long answer")]).await;
    stream(work, tx, 25, 123).await;
    send(tx, [sign]).await;
    stream(work, tx, quiet() + 1, 0).await;
    observer.finish(Ok(()));
    tick(work.as_mut(), 5).await;
}

/// A short reply of `count` audible frames, `sign` heard after `before` of
/// them; returns how many of its frames reached the device.
async fn backchannel<F: Future>(
    work: &mut Pin<Box<F>>,
    tx: &Events,
    observer: &Observer,
    (before, count): (usize, usize),
    sign: Vec<MediaEvent>,
) -> usize {
    let written = frames(observer).len();
    send(tx, [said("Mm-hm")]).await;
    stream(work, tx, before, 123).await;
    send(tx, sign).await;
    stream(work, tx, count - before, 123).await;
    send(tx, [done()]).await;
    for _ in 0..100 {
        if observer.commands().last() == Some(&Command::Stop) {
            break;
        }
        tick(work.as_mut(), 1).await;
    }
    let played = frames(observer).len() - written;
    observer.finish(Ok(()));
    tick(work.as_mut(), 5).await;
    played
}

fn cut_row(turn: u64, signal: usize) -> (&'static str, Value) {
    ("barge.cut", json!({"turn": turn, "trigger": trigger(signal)}))
}

fn rearmed_row(turn: u64, reason: &str) -> (&'static str, Value) {
    ("barge.rearmed", json!({"turn": turn, "reason": reason}))
}

fn suppressed_row(turn: u64, signal: usize, count: u64) -> (&'static str, Value) {
    (
        "barge.suppressed_in_turn",
        json!({"turn": turn, "trigger": trigger(signal), "count": count}),
    )
}

/// (a) The first talk-over cuts once, and the trace says so once.
#[tokio::test(start_paused = true)]
async fn barge_turn_first_talk_over_cuts_once_and_marks_it() {
    for signal in 0..2 {
        let observer = Observer::default();
        let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = client.events().unwrap();
        wake(&observer, 0);
        let mic = microphone(&mut events).await;
        let (mut port, tx) = port();
        let rows = Rows::default();
        let options = Options {
            listen_timeout: Some(Duration::from_secs(3)),
            ..options()
        };
        let mut work = Box::pin(run(
            &client,
            &mut events,
            mic,
            &mut port,
            options,
            std::future::pending(),
            barge_rows(&rows),
        ));
        send(&tx, [asked()]).await;
        cut_by(&mut work, &tx, &observer, heard(signal)).await;
        work.await.unwrap();
        assert_eq!((signal, cuts(&observer)), (signal, 1));
        // The open turn may re-arm as the run ends; nothing else is marked.
        let rows = rows.take();
        let barges: Vec<_> = rows.iter().filter(|row| row.0 != "barge.rearmed").cloned().collect();
        assert_eq!(barges, vec![cut_row(1, signal)], "signal {signal}");
    }
}

/// (b) After the cut the user talks on, the model backchannels and the user's
/// speech is heard again under it: the backchannel plays whole, nothing more
/// is cut, and the turn's end re-arms the cut.
#[tokio::test(start_paused = true)]
async fn barge_turn_backchannel_in_the_same_turn_plays_whole() {
    for signal in [1, 0] {
        let observer = Observer::default();
        let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = client.events().unwrap();
        wake(&observer, 0);
        let mic = microphone(&mut events).await;
        let (mut port, tx) = port();
        let rows = Rows::default();
        let options = Options {
            listen_timeout: Some(Duration::from_secs(3)),
            ..options()
        };
        let mut work = Box::pin(run(
            &client,
            &mut events,
            mic,
            &mut port,
            options,
            std::future::pending(),
            barge_rows(&rows),
        ));
        send(&tx, [asked()]).await;
        cut_by(&mut work, &tx, &observer, heard(signal)).await;
        // The user talks on; the model backchannels under it.
        send(&tx, [heard(1)]).await;
        let played = backchannel(&mut work, &tx, &observer, (3, 10), vec![heard(signal)]).await;
        assert_eq!((signal, played, cuts(&observer)), (signal, 10, 1));
        // The user's turn ends.
        send(&tx, [heard(2)]).await;
        tick(work.as_mut(), 5).await;
        work.await.unwrap();
        assert_eq!(cuts(&observer), 1);
        let mut wanted = vec![cut_row(1, signal)];
        if signal == 0 {
            // A transcript fragment of the turn that cut is that turn's own,
            // never a talk-over; a speech start is always a sign, and is passed.
            wanted.push(suppressed_row(1, signal, 1));
        }
        wanted.push(rearmed_row(1, "cleared"));
        assert_eq!(rows.take(), wanted, "signal {signal}");
    }
}

/// (c) Once the user's turn has ended, the next reply can be cut again.
#[tokio::test(start_paused = true)]
async fn barge_turn_rearms_once_the_user_turn_ends() {
    for signal in 0..2 {
        let observer = Observer::default();
        let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = client.events().unwrap();
        wake(&observer, 0);
        let mic = microphone(&mut events).await;
        let (mut port, tx) = port();
        let rows = Rows::default();
        let options = Options {
            listen_timeout: Some(Duration::from_secs(3)),
            ..options()
        };
        let mut work = Box::pin(run(
            &client,
            &mut events,
            mic,
            &mut port,
            options,
            std::future::pending(),
            barge_rows(&rows),
        ));
        send(&tx, [asked()]).await;
        cut_by(&mut work, &tx, &observer, heard(signal)).await;
        // The interrupting turn ends: its whole transcript.
        send(&tx, [heard(1), heard(2)]).await;
        tick(work.as_mut(), 5).await;
        cut_by(&mut work, &tx, &observer, heard(signal)).await;
        assert_eq!((signal, cuts(&observer)), (signal, 2));
        send(&tx, [heard(2)]).await;
        work.await.unwrap();
        let wanted = vec![
            cut_row(1, signal),
            rearmed_row(1, "cleared"),
            cut_row(2, signal),
            rearmed_row(2, "cleared"),
        ];
        assert_eq!(rows.take(), wanted, "signal {signal}");
    }
}

/// (d) The session's own end of a turn inside the user's sentence: a stop
/// and start of its speech detection, or a whole transcript and a new one.
/// The bridge follows the session: that is a new turn, so it may cut again.
#[tokio::test(start_paused = true)]
async fn barge_turn_session_turn_end_inside_a_sentence_is_a_new_turn() {
    for signal in 0..2 {
        let observer = Observer::default();
        let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = client.events().unwrap();
        wake(&observer, 0);
        let mic = microphone(&mut events).await;
        let (mut port, tx) = port();
        let rows = Rows::default();
        let options = Options {
            listen_timeout: Some(Duration::from_secs(3)),
            ..options()
        };
        let mut work = Box::pin(run(
            &client,
            &mut events,
            mic,
            &mut port,
            options,
            std::future::pending(),
            barge_rows(&rows),
        ));
        send(&tx, [asked()]).await;
        cut_by(&mut work, &tx, &observer, heard(signal)).await;
        let turn = if signal == 0 {
            vec![speech(false), speech(true)]
        } else {
            vec![heard(1), heard(2), heard(1)]
        };
        let played = backchannel(&mut work, &tx, &observer, (15, 30), turn).await;
        assert_eq!((signal, played < 30, cuts(&observer)), (signal, true, 2));
        send(&tx, [heard(2)]).await;
        work.await.unwrap();
        let wanted = vec![
            cut_row(1, signal),
            rearmed_row(1, "cleared"),
            cut_row(2, signal),
            rearmed_row(2, "cleared"),
        ];
        assert_eq!(rows.take(), wanted, "signal {signal}");
    }
}

/// (e) A speech start the session never transcribes holds the cut's turn
/// open only until the speech-start expiry (PR 326): a sign inside it is
/// passed, and the expiry re-arms the cut as an end of speech would.
#[tokio::test(start_paused = true)]
async fn barge_turn_speech_start_expiry_rearms_the_cut() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    let rows = Rows::default();
    let options = Options {
        listen_timeout: Some(Duration::from_secs(60)),
        response_deadline: Instant::now() + Duration::from_secs(60),
        ..options()
    };
    let mut work = Box::pin(run(
        &client,
        &mut events,
        mic,
        &mut port,
        options,
        std::future::pending(),
        barge_rows(&rows),
    ));
    send(&tx, [asked()]).await;
    cut_by(&mut work, &tx, &observer, heard(0)).await;
    // "Sure", under another untranscribed speech start of the same turn,
    // plays whole and then stalls.
    let written = frames(&observer).len();
    send(&tx, [said("Sure")]).await;
    stream(&mut work, &tx, 5, 123).await;
    send(&tx, [heard(0)]).await;
    stream(&mut work, &tx, 20, 123).await;
    tick(work.as_mut(), quiet() * 2).await;
    assert_eq!((frames(&observer).len() - written >= 25, cuts(&observer)), (true, 1));
    // No transcript, no end of speech: the latch expires and re-arms the cut.
    for _ in 0..21 {
        tokio::time::advance(Duration::from_secs(1)).await;
        pending(work.as_mut()).await;
    }
    stream(&mut work, &tx, 10, 123).await;
    send(&tx, [heard(0)]).await;
    stream(&mut work, &tx, quiet() + 1, 0).await;
    assert_eq!(cuts(&observer), 2);
    observer.finish(Ok(()));
    tick(work.as_mut(), 5).await;
    drop(work);
    assert_eq!(
        rows.take(),
        vec![
            cut_row(1, 0),
            suppressed_row(1, 0, 1),
            rearmed_row(1, "expired"),
            cut_row(2, 0)
        ]
    );
}

/// `voice.local_barge_cut = false`: the bridge neither holds nor cuts; the
/// model's own yield is the barge-in. The reply the user talks over plays
/// all the audio the model sent, and the trace marks the turn it left alone.
#[tokio::test(start_paused = true)]
async fn barge_turn_local_cut_off_leaves_the_yield_to_the_model() {
    use crate::bridge_world::{Controller, scope};
    use echo_core_api::SessionKnobs;
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    let rows = Rows::default();
    let options = Options {
        listen_timeout: Some(Duration::from_secs(3)),
        ..options()
    };
    let knobs = SessionKnobs {
        local_cut: false,
        ..SessionKnobs::default()
    };
    let world = Controller::for_voice_fixture();
    world.configure_session(knobs).unwrap();
    let mut work = Box::pin(scope(
        world,
        None,
        run(
            &client,
            &mut events,
            mic,
            &mut port,
            options,
            std::future::pending(),
            barge_rows(&rows),
        ),
    ));
    send(&tx, [asked()]).await;
    // The user talks over the reply; the model yields after 300 ms.
    let played = backchannel(&mut work, &tx, &observer, (25, 40), vec![heard(0), heard(1)]).await;
    assert_eq!((played, cuts(&observer)), (40, 0));
    send(&tx, [heard(2)]).await;
    work.await.unwrap();
    assert_eq!(
        rows.take(),
        vec![
            ("barge.model_only", json!({"turn": 1, "trigger": "speech_started"})),
            rearmed_row(1, "cleared"),
        ]
    );
}
