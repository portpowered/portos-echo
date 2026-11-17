use super::*;
use echo_link::{
    EchoLink, LinkConnector, LinkEvent, LinkEvents, MicFrame,
    fake::{Command, FakeConnector, Observer},
};

async fn settle(
    work: impl std::future::Future<Output = Result<crate::bridge_voice::Stats, Error>>,
    observer: &Observer,
) {
    tokio::pin!(work);
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(work.as_mut(), cx).is_pending());
        if observer.commands().contains(&Command::Stop) {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    observer.finish(Ok(()));
    let stats = work.await.unwrap();
    assert_eq!(
        (
            stats.microphone_frames,
            stats.reply_received,
            stats.reply_sent,
            stats.reply_dropped
        ),
        (2, 3, 3, 0)
    );
    let frames: Vec<_> = observer
        .commands()
        .into_iter()
        .filter_map(|c| match c {
            Command::Frame(bytes) => Some(bytes),
            _ => None,
        })
        .collect();
    assert_eq!(frames, vec![vec![0; 960]; 3]);
}

#[tokio::test(start_paused = true)]
async fn media_call_runs_device_capture_reply_drain_and_played_on_one_io_owner() {
    let (mut io, mut server) = fixture();
    let (peer, state, tx) = peer(true, None);
    let observer = Observer::default();
    let mut link = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = link.events().unwrap();
    observer.wake(
        "Computer",
        vec![MicFrame {
            format: crate::bridge_audio::format(16_000),
            payload: vec![0; 1280],
        }],
    );
    let Some(LinkEvent::Wake(run)) = events.recv().await.unwrap() else {
        unreachable!()
    };
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        call.start(&options(), vec![], deadline()).await.unwrap();
        for _ in 0..3 {
            tx.send([0; 320]).await.unwrap();
        }
        let work = Box::pin(crate::bridge_voice::run(
            &link,
            &mut events,
            run,
            &mut call,
            crate::bridge_voice::Options {
                reply_lead_ms: 200,
                response_deadline: deadline(),
                played_timeout: std::time::Duration::from_secs(1),
                muted: false,
            },
            std::future::pending(),
            |_| Ok(()),
        ));
        settle(work, &observer).await;
        assert_eq!(state.sent.lock().unwrap().len(), 2);
        call.close(deadline()).await.unwrap();
    };
    let remote = async {
        start(&mut server).await;
        write(&mut server, json!({"method":"thread/realtime/transcript/done","params":{"threadId":"thread-1","role":"assistant","text":"fixture reply"}})).await;
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
}
