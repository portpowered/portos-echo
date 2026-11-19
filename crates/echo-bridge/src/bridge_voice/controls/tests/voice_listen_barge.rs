//! T1: a settled persistent reply, another reply, a cut and its replacement.
//! Memory-only ports and paused time; native flush remains a T3 obligation.
use super::*;

#[tokio::test(start_paused = true)]
async fn voice_listen_barge_after_settled_reply_retains_run_and_fresh_idle_deadline() {
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
    let paused = Cell::new(0);
    let mut work = Box::pin(super::super::super::run(
        &client,
        &mut events,
        microphone,
        &mut port,
        Options {
            listen_timeout: Some(Duration::from_secs(3)),
            session_max: Duration::from_secs(600),
            reply_lead_ms: 200,
            response_deadline: Instant::now() + Duration::from_secs(10),
            played_timeout: Duration::from_secs(1),
            muted: false,
        },
        std::future::pending(),
        |notice| {
            match notice {
                Notice::ReplyPlayed => receipts.set(receipts.get() + 1),
                Notice::ReplyPaused => paused.set(paused.get() + 1),
                _ => (),
            }
            Ok(())
        },
    ));
    let prefix = journey(&mut work, &tx, &observer, &receipts, &paused).await;
    tokio::time::advance(Duration::from_millis(1)).await;
    let stats = work.await.unwrap();
    assert_eq!(stats.reply_sent, u64::try_from(prefix + 12).unwrap());
    assert_eq!((receipts.get(), cuts(&observer)), (2, 1));
    assert_eq!(observer.commands().last(), Some(&Command::EndRun));
    assert_replacement_pcm(&observer);
}

async fn journey(
    work: &mut Pin<Box<impl Future<Output = Result<super::super::super::Stats, Error>>>>,
    tx: &mpsc::Sender<MediaEvent>,
    observer: &Observer,
    receipts: &Cell<usize>,
    paused: &Cell<usize>,
) -> usize {
    reply(tx, 5000).await;
    settle(work, observer, receipts, 1).await;
    // Start a new answer before idle expires. Its unsent converter tail must
    // be abandoned when speech cuts it, even after the old idle would expire.
    tokio::time::advance(Duration::from_secs(2)).await;
    unfinished(tx).await;
    poll(work.as_mut()).await;
    tokio::time::advance(Duration::from_secs(2)).await;
    poll(work.as_mut()).await;
    let prefix = frames(observer);
    assert!(prefix > 12);
    cut(work, tx, observer, receipts, paused, prefix).await;
    replacement(work, tx, observer, receipts).await;
    assert_eq!(frames(observer), prefix + 12);
    before_expiry(work, observer).await;
    prefix
}

async fn cut(
    work: &mut Pin<Box<impl Future<Output = Result<super::super::super::Stats, Error>>>>,
    tx: &mpsc::Sender<MediaEvent>,
    observer: &Observer,
    receipts: &Cell<usize>,
    paused: &Cell<usize>,
    prefix: usize,
) {
    tx.send(MediaEvent::Control(Event::SpeechStarted)).await.unwrap();
    poll(work.as_mut()).await;
    assert_eq!(cuts(observer), 1);
    assert_eq!(receipts.get(), 1);
    let before = paused.get();
    observer.finish(Ok(()));
    poll(work.as_mut()).await;
    assert_eq!(paused.get(), before + 1);
    idle_noise(tx).await;
    poll(work.as_mut()).await;
    assert_eq!(frames(observer), prefix);
}

async fn before_expiry(
    work: &mut Pin<Box<impl Future<Output = Result<super::super::super::Stats, Error>>>>,
    observer: &Observer,
) {
    assert!(!observer.commands().contains(&Command::EndRun));
    tokio::time::advance(Duration::from_millis(2999)).await;
    poll(work.as_mut()).await;
    assert!(!observer.commands().contains(&Command::EndRun));
}

async fn unfinished(tx: &mpsc::Sender<MediaEvent>) {
    tx.send(MediaEvent::Control(Event::Transcript {
        role: "assistant".into(),
        text: "interrupted".into(),
        done: false,
    }))
    .await
    .unwrap();
    for _ in 0..24 {
        tx.send(MediaEvent::Audio(Box::new([9000; 320]))).await.unwrap();
    }
}

async fn replacement(
    work: &mut Pin<Box<impl Future<Output = Result<super::super::super::Stats, Error>>>>,
    tx: &mpsc::Sender<MediaEvent>,
    observer: &Observer,
    receipts: &Cell<usize>,
) {
    reply(tx, -5000).await;
    for _ in 0..50 {
        poll(work.as_mut()).await;
        if observer.commands().last() == Some(&Command::Stop) {
            break;
        }
        tokio::time::advance(Duration::from_millis(20)).await;
    }
    assert_eq!(observer.commands().last(), Some(&Command::Stop));
    observer.finish(Ok(()));
    for _ in 0..50 {
        poll(work.as_mut()).await;
        if receipts.get() == 2 {
            return;
        }
        tokio::time::advance(Duration::from_millis(20)).await;
    }
    assert_eq!(receipts.get(), 2);
}

fn frames(observer: &Observer) -> usize {
    observer
        .commands()
        .iter()
        .filter(|cmd| matches!(cmd, Command::Frame(_)))
        .count()
}

fn cuts(observer: &Observer) -> usize {
    observer
        .commands()
        .iter()
        .filter(|cmd| matches!(cmd, Command::Cut(_)))
        .count()
}

fn assert_replacement_pcm(observer: &Observer) {
    let commands = observer.commands();
    let tail = commands
        .iter()
        .rev()
        .filter_map(|cmd| {
            if let Command::Frame(bytes) = cmd {
                Some(bytes)
            } else {
                None
            }
        })
        .take(12);
    for frame in tail {
        assert_eq!(frame.len(), 960);
        let sample = i16::from_le_bytes([frame[480], frame[481]]);
        assert!((i32::from(sample) + 5000).abs() <= 10);
    }
}
