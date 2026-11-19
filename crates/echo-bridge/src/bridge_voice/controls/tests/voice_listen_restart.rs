//! T1: settings admission through real reply/listen loops, memory ports/clock.
use super::*;
use crate::bridge_settings::settings_restart::Restart;

fn options() -> Options {
    Options {
        listen_timeout: Some(Duration::from_secs(300)),
        session_max: Duration::from_secs(600),
        reply_lead_ms: 200,
        response_deadline: Instant::now() + Duration::from_secs(10),
        played_timeout: Duration::from_secs(1),
        muted: false,
    }
}

#[tokio::test(start_paused = true)]
async fn settings_restart_during_reply_waits_for_played_then_ends_without_followup() {
    let restart = Restart::default();
    restart
        .scope(async {
            let observer = Observer::default();
            let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
            let mut events = client.events().unwrap();
            observer.wake("Computer", vec![]);
            let Some(LinkEvent::Wake(microphone)) = events.recv().await.unwrap() else {
                unreachable!()
            };
            let (tx, rx) = mpsc::channel(64);
            let mut port = Port(rx, std::rc::Rc::default());
            let receipts = Cell::new(0);
            let mut work = Box::pin(super::super::super::run(
                &client,
                &mut events,
                microphone,
                &mut port,
                options(),
                std::future::pending(),
                |notice| {
                    if matches!(notice, Notice::ReplyPlayed) {
                        receipts.set(receipts.get() + 1);
                    }
                    Ok(())
                },
            ));
            reply(&tx, 5000).await;
            poll(work.as_mut()).await;
            restart.request();
            restart.request();
            before_played(&mut work, &observer).await;
            assert_eq!(receipts.get(), 0);
            observer.finish(Ok(()));
            let stats = work.await.unwrap();
            assert_eq!((stats.reply_sent, receipts.get()), (12, 1));
            assert_eq!(observer.commands().last(), Some(&Command::EndRun));
            assert_eq!(
                observer
                    .commands()
                    .iter()
                    .filter(|cmd| **cmd == Command::EndRun)
                    .count(),
                1
            );
            assert!(restart.pending());
        })
        .await;
}

async fn before_played(
    work: &mut Pin<Box<impl Future<Output = Result<super::super::super::Stats, Error>>>>,
    observer: &Observer,
) {
    for _ in 0..50 {
        poll(work.as_mut()).await;
        if observer.commands().last() == Some(&Command::Stop) {
            break;
        }
        tokio::time::advance(Duration::from_millis(20)).await;
    }
    assert_eq!(observer.commands().last(), Some(&Command::Stop));
    assert!(!observer.commands().contains(&Command::EndRun));
}

// Off plus pending replacement must still consume admitted work and its answer.
#[tokio::test(start_paused = true)]
async fn settings_restart_followup_off_waits_for_delegation_and_owed_playback() {
    let restart = Restart::default();
    restart.scope(disabled_dispatch(&restart, false)).await;
    restart.scope(disabled_dispatch(&restart, true)).await;
}

async fn disabled_dispatch(restart: &Restart, completed_first: bool) {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    observer.wake("Computer", vec![]);
    let Some(LinkEvent::Wake(microphone)) = events.recv().await.unwrap() else {
        unreachable!()
    };
    let (tx, rx) = mpsc::channel(64);
    let mut port = Port(rx, std::rc::Rc::default());
    let receipts = Cell::new(0);
    let mut options = options();
    options.listen_timeout = None;
    let mut work = Box::pin(super::super::super::run(
        &client,
        &mut events,
        microphone,
        &mut port,
        options,
        std::future::pending(),
        |notice| {
            if matches!(notice, Notice::ReplyPlayed) {
                receipts.set(receipts.get() + 1);
            }
            Ok(())
        },
    ));
    disabled_drive(&mut work, &tx, &observer, &receipts, (restart, completed_first)).await;
}

async fn disabled_drive(
    work: &mut Pin<Box<impl Future<Output = Result<super::super::super::Stats, Error>>>>,
    tx: &mpsc::Sender<MediaEvent>,
    observer: &Observer,
    receipts: &Cell<usize>,
    (restart, completed_first): (&Restart, bool),
) {
    tx.send(dispatch(true)).await.unwrap();
    reply(tx, 5000).await;
    reply(tx, 5000).await;
    restart.request();
    before_played(work, observer).await;
    release_disabled(work, tx, observer, completed_first).await;
    assert_eq!(receipts.get(), 0);
    let now = Instant::now();
    let stats = work.as_mut().await.unwrap();
    assert_eq!((stats.reply_sent, receipts.get()), (24, 1));
    assert!(now.elapsed() < Duration::from_secs(1));
    assert_eq!(
        observer
            .commands()
            .iter()
            .filter(|cmd| **cmd == Command::EndRun)
            .count(),
        1
    );
}

async fn release_disabled(
    work: &mut Pin<Box<impl Future<Output = Result<super::super::super::Stats, Error>>>>,
    tx: &mpsc::Sender<MediaEvent>,
    observer: &Observer,
    completed_first: bool,
) {
    if completed_first {
        tx.send(dispatch(false)).await.unwrap();
        poll(work.as_mut()).await;
        observer.finish(Ok(()));
    } else {
        observer.finish(Ok(()));
        settle_disabled_reply(work, observer).await;
        assert!(!observer.commands().contains(&Command::EndRun));
        tokio::time::advance(Duration::from_secs(2)).await;
        poll(work.as_mut()).await;
        tx.send(dispatch(false)).await.unwrap();
    }
}

async fn settle_disabled_reply(
    work: &mut Pin<Box<impl Future<Output = Result<super::super::super::Stats, Error>>>>,
    observer: &Observer,
) {
    // The second thinking indication follows Played and mouth drain. Advance
    // through those timers rather than skipping over the playback deadline.
    let thinking = || {
        observer
            .commands()
            .iter()
            .filter(|cmd| **cmd == Command::Thinking(true))
            .count()
    };
    for _ in 0..50 {
        poll(work.as_mut()).await;
        if thinking() == 2 {
            break;
        }
        tokio::time::advance(Duration::from_millis(20)).await;
    }
    assert_eq!(thinking(), 2);
}

fn dispatch(started: bool) -> MediaEvent {
    let (thread, turn) = ("fixture-thread".into(), "fixture-turn".into());
    MediaEvent::Control(if started {
        Event::TurnStarted { thread, turn }
    } else {
        Event::TurnCompleted { thread, turn }
    })
}
