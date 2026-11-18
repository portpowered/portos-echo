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

#[tokio::test(start_paused = true)]
async fn setup_stages_share_epoch_and_keep_only_completed_private_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let mut trace = crate::bridge_trace::Trace::open(&path, "api_key").unwrap();
    tokio::time::advance(std::time::Duration::from_millis(7)).await;
    trace.record("call.peer_created", None, json!({})).unwrap();
    let (mut io, mut server) = fixture();
    let (peer, state, _tx) = peer(false, None);
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        call.trace_setup(trace).unwrap();
        call.start(&options(), vec![], deadline()).await.unwrap();
        call.close(deadline()).await.unwrap();
    };
    let remote = async {
        start(&mut server).await;
        state.applied.notified().await;
        tokio::time::advance(std::time::Duration::from_millis(3)).await;
        for stage in [
            echo_media::SetupStage::IceConnected,
            echo_media::SetupStage::DataChannelOpen,
        ] {
            state.observer.lock().unwrap().as_ref().unwrap()(stage).unwrap();
        }
        state.states.send_replace(PeerState::Connected);
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
    let rows: Vec<serde_json::Value> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect();
    assert_eq!(
        rows.iter()
            .map(|row| row["event"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "call.peer_created",
            "call.offer_ready",
            "call.realtime_start_sent",
            "call.answer_received",
            "call.answer_applied",
            "call.ice_connected",
            "call.data_channel_open",
            "call.live_connected",
        ]
    );
    assert_eq!(
        rows.iter()
            .map(|row| row["elapsed_ms"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [7, 7, 7, 7, 7, 10, 10, 10]
    );
    for row in rows {
        assert_eq!(row.as_object().unwrap().len(), 3);
        assert_eq!(row["provider_auth_mode"], "api_key");
    }
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn setup_expired_deadline_closes_without_offer_or_rpc() {
    let (mut io, server) = fixture();
    let (peer, state, _tx) = peer(true, None);
    let mut call = MediaCall::new(&mut io, peer);
    assert_eq!(
        call.start(&options(), vec![], Instant::now()).await,
        Err(Error::Deadline)
    );
    assert_eq!(state.offers.load(Ordering::SeqCst), 0);
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    assert_eq!(call.send([0; 320]), Err(Error::Unavailable));
    call.close(deadline()).await.unwrap();
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    drop(call);
    assert_eq!(
        io.begin_request("account/read", &json!({})),
        Err(echo_codex::codex_rpc::RpcError::Disconnected)
    );
    let mut bytes = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut server.into_inner(), &mut bytes)
        .await
        .unwrap();
    assert_eq!(bytes, Vec::<u8>::new());
}
