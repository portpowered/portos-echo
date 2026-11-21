//! T1: the no-answer guard on the real run loop, memory-only ports, paused time.
//! The window runs from the user's end of speech; a resolved hand-off holds
//! the run for its answer; a silent model still ends the run.
use super::*;
use std::cell::Cell;

const WINDOW: Duration = Duration::from_secs(60);
const GRACE: Duration = voice_wait::HANDBACK_GRACE;

fn guard(listen_timeout: Option<Duration>) -> Options {
    Options {
        listen_timeout,
        session_max: Duration::from_secs(600),
        reply_lead_ms: 200,
        response_deadline: Instant::now() + WINDOW,
        played_timeout: Duration::from_secs(5),
        muted: false,
    }
}

fn speech(started: bool) -> MediaEvent {
    MediaEvent::Control(if started {
        Event::SpeechStarted
    } else {
        Event::SpeechStopped
    })
}

/// Hold the run pending across `seconds` of the paused clock.
async fn wait<F: Future>(mut work: Pin<&mut F>, seconds: u64) {
    pending(work.as_mut()).await;
    tokio::time::advance(Duration::from_secs(seconds)).await;
    pending(work.as_mut()).await;
}

/// Pace the queued answer out, then give the speaker's Played receipt.
async fn play<F: Future>(mut work: Pin<&mut F>, observer: &Observer) {
    for _ in 0..64 {
        if observer.commands().last() == Some(&Command::Stop) {
            break;
        }
        tick(work.as_mut(), 1).await;
    }
    assert_eq!(observer.commands().last(), Some(&Command::Stop));
    observer.finish(Ok(()));
}

fn answer() -> Vec<MediaEvent> {
    let mut events = vec![said("It's three.")];
    events.extend(audible(12));
    events.push(done());
    events
}

fn cuts(observer: &Observer) -> usize {
    observer
        .commands()
        .iter()
        .filter(|command| matches!(command, Command::Cut(_)))
        .count()
}

fn count(observer: &Observer, wanted: &Command) -> usize {
    observer.commands().iter().filter(|command| *command == wanted).count()
}

/// The call seen on the Echo: 44 s of question, a hand-off that resolves at
/// 54 s and an answer that starts at 62 s, past 60 s from the wake.
#[tokio::test(start_paused = true)]
async fn no_answer_long_question_and_slow_handoff_still_play_the_answer() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    let played = Cell::new(0);
    let start = Instant::now();
    let mut work = Box::pin(run(
        &client,
        &mut events,
        mic,
        &mut port,
        guard(None),
        std::future::pending(),
        |notice| {
            played.set(played.get() + usize::from(matches!(notice, Notice::ReplyPlayed)));
            Ok(())
        },
    ));
    send(&tx, [speech(true)]).await;
    wait(work.as_mut(), 44).await;
    send(&tx, [speech(false)]).await;
    wait(work.as_mut(), 1).await;
    send(&tx, [turn(true)]).await;
    wait(work.as_mut(), 9).await;
    send(&tx, [turn(false)]).await;
    wait(work.as_mut(), 8).await;
    assert_eq!(Instant::now() - start, Duration::from_secs(62));
    send(&tx, answer()).await;
    play(work.as_mut(), &observer).await;
    let stats = work.await.unwrap();
    assert_eq!((stats.reply_received, stats.reply_sent), (12, 12));
    assert_eq!(played.get(), 1);
    assert_eq!(count(&observer, &Command::EndRun), 1);
    assert_eq!(cuts(&observer), 0);
}

/// The guard never fires while the user is still speaking, however long.
#[tokio::test(start_paused = true)]
async fn no_answer_never_fires_while_the_user_is_still_speaking() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    let played = Cell::new(0);
    let mut work = Box::pin(run(
        &client,
        &mut events,
        mic,
        &mut port,
        guard(None),
        std::future::pending(),
        |notice| {
            played.set(played.get() + usize::from(matches!(notice, Notice::ReplyPlayed)));
            Ok(())
        },
    ));
    send(&tx, [speech(true)]).await;
    wait(work.as_mut(), 65).await;
    send(&tx, [speech(false)]).await;
    wait(work.as_mut(), 2).await;
    send(&tx, answer()).await;
    play(work.as_mut(), &observer).await;
    assert_eq!(work.await.unwrap().reply_sent, 12);
    assert_eq!(played.get(), 1);
    assert_eq!(cuts(&observer), 0);
}

/// A hand-off resolved and its answer never starts. The run is unanswered at
/// the end of the no-answer window, and no sooner than the grace allows.
#[tokio::test(start_paused = true)]
async fn no_answer_resolved_handoff_without_an_answer_ends_after_its_grace() {
    for (resolved, ended) in [(54, 44 + 60), (100, 100 + 20)] {
        assert_eq!(GRACE, Duration::from_secs(20));
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
            guard(None),
            std::future::pending(),
            |notice| {
                assert!(!matches!(notice, Notice::ReplyPlayed));
                Ok(())
            },
        ));
        send(&tx, [speech(true)]).await;
        wait(work.as_mut(), 44).await;
        send(&tx, [speech(false), turn(true)]).await;
        wait(work.as_mut(), resolved - 44).await;
        send(&tx, [turn(false)]).await;
        // One nanosecond short of the deadline the run is still held.
        let held = Duration::from_secs(ended - resolved).saturating_sub(Duration::from_nanos(1));
        pending(work.as_mut()).await;
        tokio::time::advance(held).await;
        pending(work.as_mut()).await;
        tokio::time::advance(Duration::from_nanos(1)).await;
        assert_eq!(work.await, Err(Error::NoResponse), "resolved at {resolved} s");
        assert_eq!(Instant::now() - start, Duration::from_secs(ended));
        assert_eq!(count(&observer, &Command::EndRun), 0);
    }
}

/// A silent model is unanswered one window after the user stops speaking, and
/// one window after the wake when no speech was ever signalled.
#[tokio::test(start_paused = true)]
async fn no_answer_silent_model_ends_one_window_after_speech_stops() {
    for spoken in [0, 44] {
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
            guard(None),
            std::future::pending(),
            |_| Ok(()),
        ));
        if spoken > 0 {
            send(&tx, [speech(true)]).await;
            wait(work.as_mut(), spoken).await;
            send(&tx, [speech(false)]).await;
        }
        pending(work.as_mut()).await;
        tokio::time::advance(WINDOW.saturating_sub(Duration::from_nanos(1))).await;
        pending(work.as_mut()).await;
        tokio::time::advance(Duration::from_nanos(1)).await;
        assert_eq!(work.await, Err(Error::NoResponse), "spoke for {spoken} s");
        assert_eq!(Instant::now() - start, Duration::from_secs(spoken) + WINDOW);
        assert_eq!(cuts(&observer), 1);
        assert_eq!(count(&observer, &Command::EndRun), 0);
    }
}

/// After a reply, speech the model never answers is not a failed run: one
/// window after it stops, the run ends once, the usual way.
#[tokio::test(start_paused = true)]
async fn no_answer_unanswered_follow_up_speech_ends_the_run_without_a_failure() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    let played = Cell::new(0);
    let mut work = Box::pin(run(
        &client,
        &mut events,
        mic,
        &mut port,
        guard(Some(Duration::from_secs(15))),
        std::future::pending(),
        |notice| {
            played.set(played.get() + usize::from(matches!(notice, Notice::ReplyPlayed)));
            Ok(())
        },
    ));
    send(&tx, answer()).await;
    play(work.as_mut(), &observer).await;
    for _ in 0..64 {
        if played.get() == 1 {
            break;
        }
        tick(work.as_mut(), 1).await;
    }
    assert_eq!(played.get(), 1);
    // One second into the listen window the user is heard, and nothing answers.
    wait(work.as_mut(), 1).await;
    send(&tx, [speech(true)]).await;
    wait(work.as_mut(), 2).await;
    send(&tx, [speech(false)]).await;
    pending(work.as_mut()).await;
    let stopped = Instant::now();
    tokio::time::advance(WINDOW.saturating_sub(Duration::from_nanos(1))).await;
    pending(work.as_mut()).await;
    assert_eq!(count(&observer, &Command::EndRun), 0);
    tokio::time::advance(Duration::from_nanos(1)).await;
    play(work.as_mut(), &observer).await;
    let stats = work.await.unwrap();
    assert!(Instant::now() - stopped < WINDOW + Duration::from_secs(5));
    assert_eq!(stats.reply_sent, 12);
    assert_eq!(played.get(), 1);
    assert_eq!(count(&observer, &Command::EndRun), 1);
    assert_eq!(cuts(&observer), 0);
}

/// The call seen on the Echo: a story that takes longer to tell than the
/// no-answer window. Its audio arrives as it is spoken, and it plays to the end.
#[tokio::test(start_paused = true)]
async fn no_answer_window_never_ends_a_reply_that_is_being_spoken() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    wake(&observer, 0);
    let mic = microphone(&mut events).await;
    let (mut port, tx) = port();
    let played = Cell::new(0);
    let start = Instant::now();
    let mut work = Box::pin(run(
        &client,
        &mut events,
        mic,
        &mut port,
        guard(None),
        std::future::pending(),
        |notice| {
            played.set(played.get() + usize::from(matches!(notice, Notice::ReplyPlayed)));
            Ok(())
        },
    ));
    send(&tx, [said("Once upon a time")]).await;
    for _ in 0..140 {
        send(&tx, audible(25)).await;
        tick(work.as_mut(), 25).await;
    }
    assert_eq!(Instant::now() - start, Duration::from_secs(70));
    send(&tx, [done()]).await;
    play(work.as_mut(), &observer).await;
    let stats = work.await.unwrap();
    // Every frame received was played; the pacer may add a few of its own.
    assert_eq!(stats.reply_received, 3500);
    assert!((3500..3510).contains(&stats.reply_sent), "{}", stats.reply_sent);
    assert_eq!(played.get(), 1);
    assert_eq!(count(&observer, &Command::EndRun), 1);
    assert_eq!(cuts(&observer), 0);
}

/// A reply that stops mid-speech and never finishes is unanswered one window
/// after its last sound, not one window after the wake.
#[tokio::test(start_paused = true)]
async fn no_answer_window_restarts_at_the_last_reply_audio_and_still_ends_a_stalled_reply() {
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
        guard(None),
        std::future::pending(),
        |notice| {
            assert!(!matches!(notice, Notice::ReplyPlayed));
            Ok(())
        },
    ));
    wait(work.as_mut(), 50).await;
    send(&tx, [said("Once upon")]).await;
    for _ in 0..10 {
        send(&tx, audible(25)).await;
        tick(work.as_mut(), 25).await;
    }
    // 55 s after the wake the reply has gone quiet. The first window would
    // have ended it at 60 s.
    assert_eq!(Instant::now() - start, Duration::from_secs(55));
    wait(work.as_mut(), 55).await;
    assert_eq!(work.await, Err(Error::NoResponse));
    let ended = Instant::now() - start;
    assert!(
        (Duration::from_secs(114)..=Duration::from_secs(116)).contains(&ended),
        "ended {ended:?} after the wake"
    );
    assert_eq!(count(&observer, &Command::EndRun), 0);
}
