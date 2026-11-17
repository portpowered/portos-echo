//! T1: admission and cleanup observations through memory-only component ports.
use super::*;
use crate::bridge_voice::{ConversationOptions, open_conversation};

fn opening_options() -> ConversationOptions<'static> {
    ConversationOptions {
        call: options(),
        tools: vec![],
        deadline: deadline(),
        session: session_options(),
    }
}

#[tokio::test(start_paused = true)]
async fn opening_rejects_expiry_and_ready_cancellation_before_offer_or_rpc() {
    for cancelled in [false, true] {
        let (mut io, _server) = fixture();
        let (peer, state, _audio) = peer(true, None);
        let (link, mut events, microphone, observer) = device().await;
        let mut options = opening_options();
        options.deadline = Instant::now();
        let result = open_conversation(
            (&link, &mut events, microphone),
            &mut io,
            peer,
            options,
            async {
                if !cancelled {
                    std::future::pending::<()>().await;
                }
            },
            |_| unreachable!(),
        )
        .await;
        assert_eq!(result, Err(if cancelled { Error::Control } else { Error::Deadline }));
        assert_eq!(*state.states.borrow(), PeerState::Closed);
        assert_eq!(state.offers.load(Ordering::SeqCst), 0);
        assert_eq!(*state.answers.lock().unwrap(), Vec::<String>::new());
        assert_eq!(state.closes.load(Ordering::SeqCst), 1);
        assert_eq!(observer.commands(), [Command::Connect]);
        // No request was issued, and a fresh IO generation remains usable.
        assert_eq!(
            io.begin_request("account/read", &json!({})).unwrap(),
            echo_codex::codex_protocol::RequestId::Number(1)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn opening_waits_for_media_before_connected_and_settles_played_before_completion() {
    let (mut io, mut server) = fixture();
    let (peer, state, _audio) = peer(false, None);
    let (link, mut events, microphone, observer) = device().await;
    let notices = std::cell::RefCell::new(Vec::new());
    let client = open_conversation(
        (&link, &mut events, microphone),
        &mut io,
        peer,
        opening_options(),
        std::future::pending(),
        |notice| {
            match notice {
                SessionNotice::Connected => notices.borrow_mut().push("connected"),
                SessionNotice::Started => notices.borrow_mut().push("started"),
                SessionNotice::Completed(outcome) => {
                    assert_eq!(outcome.microphone_frames, 1);
                    assert_eq!(outcome.reply_sent, 0);
                    notices.borrow_mut().push("completed");
                }
                SessionNotice::Run(_) => (),
            }
            Ok(())
        },
    );
    let remote = async {
        start(&mut server).await;
        reply(&mut server).await; // preserved while ICE is not ready
        stop(&mut server).await;
    };
    let mut work = Box::pin(async { tokio::join!(client, remote).0 });
    pump(work.as_mut()).await;
    assert_eq!(*notices.borrow(), Vec::<&str>::new());
    assert_eq!(*state.answers.lock().unwrap(), ["fixture-answer"]);
    assert_eq!(observer.commands(), [Command::Connect]);
    state.states.send_replace(PeerState::Connected);
    pump(work.as_mut()).await;
    assert_eq!(*notices.borrow(), ["connected", "started"]);
    assert_eq!(observer.commands().last(), Some(&Command::Stop));
    observer.finish(Ok(()));
    pump(work.as_mut()).await;
    assert_eq!(*notices.borrow(), ["connected", "started", "completed"]);
    tokio::time::advance(Duration::from_secs(5)).await;
    assert_eq!(work.await, Ok(1));
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    assert!(io.begin_request("account/read", &json!({})).is_ok());
}

#[tokio::test(start_paused = true)]
async fn opening_connected_observer_failure_stops_call_without_starting_playback() {
    let (mut io, mut server) = fixture();
    let (peer, state, _audio) = peer(true, None);
    let (link, mut events, microphone, observer) = device().await;
    let mut connected = 0;
    let client = open_conversation(
        (&link, &mut events, microphone),
        &mut io,
        peer,
        opening_options(),
        std::future::pending(),
        |notice| {
            assert!(matches!(notice, SessionNotice::Connected));
            connected += 1;
            Err(Error::Trace)
        },
    );
    let remote = async {
        start(&mut server).await;
        stop(&mut server).await;
    };
    assert_eq!(tokio::join!(client, remote).0, Err(Error::Trace));
    assert_eq!(connected, 1);
    assert_eq!(observer.commands(), [Command::Connect]);
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    assert!(io.begin_request("account/read", &json!({})).is_ok());
}

#[tokio::test(start_paused = true)]
async fn opening_cancelled_during_ice_closes_generation_and_media_without_connected() {
    let (mut io, mut server) = fixture();
    let (peer, state, _audio) = peer(false, None);
    let (link, mut events, microphone, observer) = device().await;
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let client = open_conversation(
        (&link, &mut events, microphone),
        &mut io,
        peer,
        opening_options(),
        async {
            cancelled.await.unwrap();
        },
        |_| unreachable!(),
    );
    let remote = async {
        start(&mut server).await;
        std::future::pending::<()>().await;
    };
    let mut work = Box::pin(async {
        tokio::select! { result = client => result, () = remote => unreachable!() }
    });
    pump(work.as_mut()).await;
    assert_eq!(*state.answers.lock().unwrap(), ["fixture-answer"]);
    cancel.send(()).unwrap();
    assert_eq!(work.await, Err(Error::Control));
    assert_eq!(observer.commands(), [Command::Connect]);
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    assert_eq!(
        io.begin_request("account/read", &json!({})),
        Err(echo_codex::codex_rpc::RpcError::Disconnected)
    );
}

#[tokio::test(start_paused = true)]
async fn opening_failed_media_or_ice_deadline_never_publishes_connected() {
    for failure in [Some(echo_media::Error::InvalidSdp), None] {
        let (mut io, mut server) = fixture();
        let (peer, state, _audio) = peer(false, failure);
        let (link, mut events, microphone, observer) = device().await;
        let began = Instant::now();
        let client = open_conversation(
            (&link, &mut events, microphone),
            &mut io,
            peer,
            opening_options(),
            std::future::pending(),
            |_| unreachable!(),
        );
        let remote = async {
            start(&mut server).await;
            std::future::pending::<()>().await;
        };
        let result = tokio::select! { result = client => result, () = remote => unreachable!() };
        assert_eq!(
            result,
            Err(if failure.is_some() {
                Error::Decode
            } else {
                Error::Deadline
            })
        );
        assert_eq!(
            Instant::now() - began,
            if failure.is_some() {
                Duration::ZERO
            } else {
                Duration::from_secs(5)
            }
        );
        assert_eq!(observer.commands(), [Command::Connect]);
        assert_eq!(state.closes.load(Ordering::SeqCst), 1);
        assert_eq!(
            io.begin_request("account/read", &json!({})),
            Err(echo_codex::codex_rpc::RpcError::Disconnected)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn opening_invalid_response_and_played_budgets_have_no_call_effects() {
    for (response, played) in [
        (Duration::ZERO, Duration::from_secs(1)),
        (Duration::from_secs(1), Duration::ZERO),
        (Duration::MAX, Duration::from_secs(1)),
    ] {
        let (mut io, _server) = fixture();
        let (peer, state, _audio) = peer(true, None);
        let (link, mut events, microphone, observer) = device().await;
        let mut options = opening_options();
        options.session.response_timeout = response;
        options.session.played_timeout = played;
        assert_eq!(
            open_conversation(
                (&link, &mut events, microphone),
                &mut io,
                peer,
                options,
                std::future::pending(),
                |_| unreachable!(),
            )
            .await,
            Err(Error::Input)
        );
        assert_eq!(*state.answers.lock().unwrap(), Vec::<String>::new());
        assert_eq!(state.closes.load(Ordering::SeqCst), 1);
        assert_eq!(observer.commands(), [Command::Connect]);
        assert_eq!(state.offers.load(Ordering::SeqCst), 0);
        assert_eq!(
            io.begin_request("account/read", &json!({})).unwrap(),
            echo_codex::codex_protocol::RequestId::Number(1)
        );
    }
}
