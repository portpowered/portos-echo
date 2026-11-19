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
