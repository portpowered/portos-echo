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
                listen_timeout: None,
                session_max: std::time::Duration::from_secs(600),
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

async fn setup_realtime(server: &mut Server) {
    let request = read(server).await;
    assert_eq!(request["method"], "thread/realtime/start");
    assert_eq!(
        request["params"],
        json!({"threadId":"thread-1", "outputModality":"audio",
            "transport":{"type":"webrtc","sdp":"fixture-offer"}, "version":"v3",
            "includeStartupContext":false, "clientManagedHandoffs":false,
            "voice":"alloy", "prompt":"Live prompt"})
    );
    notification(
        server,
        "thread/realtime/sdp",
        json!({"threadId":"thread-1","sdp":"fixture-answer"}),
    )
    .await;
    write(server, json!({"id":request["id"],"result":{}})).await;
}
#[tokio::test(start_paused = true)]
async fn setup_thread_preparation_finishes_before_held_offer_and_realtime_waits_for_both() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let trace = crate::bridge_trace::Trace::open(&path, "api_key").unwrap();
    let (mut io, mut server) = fixture();
    let (peer, state, _tx) = peer(true, None);
    let gate = Arc::new(tokio::sync::Notify::new());
    *state.offer_gate.lock().unwrap() = Some(gate.clone());
    let began = Instant::now();
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        call.trace_setup(trace).unwrap();
        call.start(&options(), vec![], deadline()).await.unwrap();
        assert_eq!(began.elapsed(), std::time::Duration::from_millis(30));
        call.close(deadline()).await.unwrap();
    };
    let remote = async {
        let request = read(&mut server).await;
        assert_eq!(request["method"], "thread/start");
        let mut expected = serde_json::to_value(echo_codex::ThreadSettings::new(vec![])).unwrap();
        expected["baseInstructions"] = options().instructions.into();
        expected["model"] = options().model.unwrap().into();
        assert_eq!(request["params"], expected);
        assert_eq!(state.offers.load(Ordering::SeqCst), 1);
        assert_eq!(began.elapsed(), std::time::Duration::ZERO);
        tokio::time::advance(std::time::Duration::from_millis(20)).await;
        write(
            &mut server,
            json!({"id":request["id"],"result":{"thread":{"id":"thread-1"}}}),
        )
        .await;
        {
            let next = read(&mut server);
            tokio::pin!(next);
            std::future::poll_fn(|cx| {
                assert!(std::future::Future::poll(next.as_mut(), cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
        }
        tokio::time::advance(std::time::Duration::from_millis(10)).await;
        gate.notify_one();
        setup_realtime(&mut server).await;
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
    let rows: Vec<serde_json::Value> = std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect();
    assert_eq!(rows[0]["event"], "call.offer_ready");
    assert_eq!(rows[0]["elapsed_ms"], 30);
    assert_eq!(rows[1]["event"], "call.realtime_start_sent");
    assert_eq!(rows[1]["elapsed_ms"], 30);
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
}
#[tokio::test(start_paused = true)]
async fn setup_preparation_failure_drops_pending_sibling_and_closes_generation() {
    for side in ["offer", "thread", "cancel", "deadline"] {
        let (mut io, mut server) = fixture();
        let (peer, state, _tx) = peer(true, None);
        let gate = Arc::new(tokio::sync::Notify::new());
        *state.offer_gate.lock().unwrap() = Some(gate.clone());
        let (cancel, cancelled) = tokio::sync::oneshot::channel::<()>();
        let client = async {
            let mut call = MediaCall::new(&mut io, peer);
            let profile = options();
            let result = tokio::select! {
                result = call.start(&profile, vec![], deadline()) => result,
                () = async { cancelled.await.unwrap(); } => Err(Error::Control),
            };
            assert_eq!(
                result,
                Err(match side {
                    "cancel" => Error::Control,
                    "deadline" => Error::Deadline,
                    _ => Error::Unavailable,
                })
            );
            call.close(deadline()).await.unwrap();
            assert_eq!(call.send([0; 320]), Err(Error::Unavailable));
            assert_eq!(call.start(&options(), vec![], deadline()).await, Err(Error::Input));
        };
        let remote = async {
            let request = read(&mut server).await;
            assert_eq!(request["method"], "thread/start");
            match side {
                "offer" => {
                    *state.offer_error.lock().unwrap() = Some(echo_media::Error::Transport);
                    gate.notify_one();
                }
                "thread" => {
                    write(
                        &mut server,
                        json!({"id":request["id"],"error":{"code":-1,"message":"fixture"}}),
                    )
                    .await;
                }
                "cancel" => cancel.send(()).unwrap(),
                _ => (),
            }
            std::future::pending::<()>().await;
        };
        tokio::select! { () = client => (), () = remote => unreachable!() }
        gate.notify_one(); // A dropped offer cannot resume or publish readiness.
        assert_eq!(state.closes.load(Ordering::SeqCst), 1);
        assert_eq!(state.offers.load(Ordering::SeqCst), 1);
        assert_eq!(*state.answers.lock().unwrap(), Vec::<String>::new());
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
}
#[tokio::test(start_paused = true)]
async fn setup_callback_recording_failure_returns_trace_after_cleanup_and_keeps_prior_bytes() {
    static OVERSIZED_MODE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    for stage in [
        echo_media::SetupStage::IceConnected,
        echo_media::SetupStage::DataChannelOpen,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let fault_path = directory.path().join("fault");
        std::fs::write(&fault_path, b"prior record\n").unwrap();
        // Inject a recording fault through the existing size guard, before write.
        let fault =
            crate::bridge_trace::Trace::open(&fault_path, OVERSIZED_MODE.get_or_init(|| "x".repeat(4096))).unwrap();
        let trace = crate::bridge_trace::Trace::open(&path, "api_key").unwrap();
        let (mut io, mut server) = fixture();
        let (peer, state, _tx) = peer(false, None);
        let client = async {
            let mut call = MediaCall::new(&mut io, peer);
            call.trace_setup(fault).unwrap();
            // Only the actual media observer uses the failing recorder.
            call.call.setup_trace = Some(trace.clone());
            call.trace = Some(trace);
            assert_eq!(call.start(&options(), vec![], deadline()).await, Err(Error::Trace));
            assert_eq!(call.send([0; 320]), Err(Error::Unavailable));
            call.close(deadline()).await.unwrap();
        };
        let remote = async {
            start(&mut server).await;
            state.applied.notified().await;
            assert_eq!(
                state.observer.lock().unwrap().as_ref().unwrap()(stage),
                Err(echo_media::Error::Transport)
            );
            state.states.send_replace(PeerState::Failed);
        };
        tokio::join!(client, remote);
        assert_eq!(state.closes.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read(fault_path).unwrap(), b"prior record\n");
        let rows: Vec<serde_json::Value> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|row| serde_json::from_str(row).unwrap())
            .collect();
        assert_eq!(
            rows.iter()
                .map(|row| row["event"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "call.offer_ready",
                "call.realtime_start_sent",
                "call.answer_received",
                "call.answer_applied",
                "call.start_failed"
            ]
        );
        assert_eq!(
            io.begin_request("account/read", &json!({})),
            Err(echo_codex::codex_rpc::RpcError::Disconnected)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn replacement_waits_for_receive_ack_across_cancelled_polls_and_reuses_call() {
    let (mut io, mut server) = fixture();
    let (peer, state, tx) = peer(true, None);
    let gate = Arc::new(tokio::sync::Notify::new());
    *state.reset_gate.lock().unwrap() = Some(gate.clone());
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        call.start(&options(), vec![], deadline()).await.unwrap();
        for expected in 1..=3 {
            call.retire_audio();
            call.retire_audio();
            let boundary = MediaEvent::Control(Event::Transcript {
                role: "assistant".into(),
                text: "same replacement".into(),
                done: false,
            });
            // Store a real ordered control boundary, then cancel its reset wait.
            let mut pending = Box::pin(call.accept_media(boundary));
            std::future::poll_fn(|cx| {
                assert!(std::future::Future::poll(pending.as_mut(), cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            drop(pending);
            tx.send([123; 320]).await.unwrap();
            assert_eq!(state.resets.load(Ordering::SeqCst), expected - 1);
            gate.notify_one();
            assert!(
                matches!(call.next().await.unwrap(), MediaEvent::Control(Event::Transcript { text, .. }) if text == "same replacement")
            );
            assert_eq!(state.resets.load(Ordering::SeqCst), expected);
            assert_eq!(call.ready_audio().await.unwrap(), None);
            tx.send([456; 320]).await.unwrap();
            assert!(matches!(call.next().await.unwrap(), MediaEvent::Audio(frame) if *frame == [456;320]));
        }
        call.close(deadline()).await.unwrap();
        assert_eq!(state.offers.load(Ordering::SeqCst), 1);
        assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    };
    let remote = async {
        start(&mut server).await;
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
}

#[tokio::test(start_paused = true)]
async fn ordinary_single_and_tool_replies_preserve_pcm_without_receive_reset() {
    let (mut io, mut server) = fixture();
    let (peer, state, tx) = peer(true, None);
    let completed = tokio::sync::Notify::new();
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        call.start(&options(), vec![], deadline()).await.unwrap();
        for delegated in [false, true] {
            if delegated {
                assert!(matches!(
                    call.next().await.unwrap(),
                    MediaEvent::Control(Event::TurnStarted { .. })
                ));
                assert!(matches!(
                    call.next().await.unwrap(),
                    MediaEvent::Control(Event::TurnCompleted { .. })
                ));
            }
            assert!(matches!(
                call.next().await.unwrap(),
                MediaEvent::Control(Event::Transcript { .. })
            ));
            let mut reply = crate::bridge_codex::ReplyAudio::new(200).unwrap();
            for _ in 0..10 {
                tx.send([456; 320]).await.unwrap();
                let MediaEvent::Audio(frame) = call.next().await.unwrap() else {
                    unreachable!()
                };
                reply.push(&frame).unwrap();
            }
            reply.finish().unwrap();
            let mut bytes = 0;
            while let Some(frame) = reply.next_frame().await {
                bytes += frame.len();
            }
            assert_eq!((bytes, state.resets.load(Ordering::SeqCst)), (9600, 0));
            completed.notify_one();
        }
        call.close(deadline()).await.unwrap();
    };
    tokio::join!(client, ordinary_replies(&mut server, &completed));
}

async fn ordinary_replies(server: &mut Server, completed: &tokio::sync::Notify) {
    start(server).await;
    for delegated in [false, true] {
        if delegated {
            notification(
                server,
                "turn/started",
                json!({"threadId":"thread-1","turn":{"id":"turn-normal"}}),
            )
            .await;
            notification(
                server,
                "turn/completed",
                json!({"threadId":"thread-1","turn":{"id":"turn-normal"}}),
            )
            .await;
        }
        notification(
            server,
            "thread/realtime/transcript/delta",
            json!({"threadId":"thread-1","role":"assistant","delta":"answer"}),
        )
        .await;
        completed.notified().await;
    }
    stop(server).await;
}
