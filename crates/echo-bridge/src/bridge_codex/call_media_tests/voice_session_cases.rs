//! T1: conversation owner, controlled JSONL/media/device ports, paused time.
#[path = "voice_open_cases.rs"]
mod opening;
#[path = "voice_wake_time.rs"]
mod wake_time;
use super::*;
use crate::bridge_voice::{SessionNotice, SessionOptions, conversation};
use echo_link::{
    EchoLink, LinkConnector, MicFrame,
    fake::{Command, FakeConnector, Observer},
};
use std::{
    future::{Future, poll_fn},
    pin::Pin,
    task::Poll,
    time::Duration,
};

fn session_options() -> SessionOptions {
    SessionOptions {
        listen_timeout: None,
        session_max: Duration::from_secs(600),
        idle: Duration::from_secs(5),
        response_timeout: Duration::from_secs(10),
        played_timeout: Duration::from_secs(1),
        reply_lead_ms: 200,
        muted: false,
    }
}

#[tokio::test(start_paused = true)]
async fn session_listening_projects_two_replies_before_one_run_and_microphone_completion() {
    let (mut io, mut server) = fixture();
    let (peer, state, _audio) = peer(true, None);
    let (link, mut events, microphone, observer) = device().await;
    let notices = std::cell::RefCell::new(Vec::new());
    let (send, receive) = mpsc::unbounded_channel();
    let mut options = session_options();
    options.listen_timeout = Some(Duration::from_secs(3));
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        call.start(&super::options(), vec![], deadline()).await.unwrap();
        conversation(
            (&link, &mut events, microphone),
            &mut call,
            options,
            std::future::pending(),
            |notice| {
                persistent_notice(notice, &notices);
                Ok(())
            },
        )
        .await
    };
    let remote = persistent_replies(&mut server, receive);
    let mut work = Box::pin(async { tokio::join!(client, remote).0 });
    for number in 1..=2 {
        send.send(()).unwrap();
        pump(work.as_mut()).await;
        assert_eq!(observer.commands().last(), Some(&Command::Stop));
        observer.finish(Ok(()));
        pump(work.as_mut()).await;
        let expected = if number == 1 {
            vec!["started", "reply"]
        } else {
            vec!["started", "reply", "reply"]
        };
        assert_eq!(*notices.borrow(), expected);
        assert!(!observer.commands().contains(&Command::EndRun));
        tokio::time::advance(Duration::from_secs(2)).await;
    }
    tokio::time::advance(Duration::from_secs(1)).await;
    pump(work.as_mut()).await;
    assert_eq!(*notices.borrow(), ["started", "reply", "reply", "microphone", "run"]);
    assert_eq!(
        observer
            .commands()
            .iter()
            .filter(|c| matches!(c, Command::EndRun))
            .count(),
        1
    );
    assert_eq!(state.closes.load(Ordering::SeqCst), 0);
    tokio::time::advance(Duration::from_secs(5)).await;
    assert_eq!(work.await, Ok(1));
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
}

fn persistent_notice(notice: SessionNotice, notices: &std::cell::RefCell<Vec<&str>>) {
    match notice {
        SessionNotice::Started => notices.borrow_mut().push("started"),
        SessionNotice::ReplyCompleted(stats) => {
            assert_eq!((stats.reply_received, stats.reply_sent), (0, 0));
            notices.borrow_mut().push("reply");
        }
        SessionNotice::MicrophoneCompleted(_) => notices.borrow_mut().push("microphone"),
        SessionNotice::Completed(_) => notices.borrow_mut().push("run"),
        _ => (),
    }
}

async fn persistent_replies(server: &mut Server, mut receive: mpsc::UnboundedReceiver<()>) {
    start(server).await;
    for _ in 0..2 {
        receive.recv().await.unwrap();
        notification(
            server,
            "thread/realtime/transcript/delta",
            json!({
                "threadId":"thread-1", "role":"assistant", "delta":"fixture"
            }),
        )
        .await;
        reply(server).await;
    }
    stop(server).await;
}

fn wake(observer: &Observer) {
    observer.wake(
        "Computer",
        vec![MicFrame {
            format: crate::bridge_audio::format(16_000),
            payload: vec![0; 640].into(),
        }],
    );
}

async fn microphone<E: echo_link::LinkEvents>(events: &mut E) -> E::Run {
    match events.recv().await.unwrap().unwrap() {
        echo_link::LinkEvent::Wake(run) => run,
        _ => unreachable!(),
    }
}

async fn pump<F: Future>(mut work: Pin<&mut F>) {
    for _ in 0..1024 {
        poll_fn(|cx| {
            assert!(work.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }
}

async fn reply(server: &mut Server) {
    notification(
        server,
        "thread/realtime/transcript/done",
        json!({"threadId":"thread-1","role":"assistant","text":"fixture reply"}),
    )
    .await;
}

async fn device() -> (
    echo_link::fake::FakeLink,
    echo_link::fake::Events,
    echo_link::fake::FakeRun,
    Observer,
) {
    let observer = Observer::default();
    let mut link = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = link.events().unwrap();
    wake(&observer);
    let microphone = microphone(&mut events).await;
    (link, events, microphone, observer)
}

#[tokio::test(start_paused = true)]
async fn session_idle_process_eof_is_failure_without_replay_and_closes_media_once() {
    let (mut io, mut server) = fixture();
    let (peer, state, _audio) = peer(true, None);
    let (link, mut events, microphone, observer) = device().await;
    let (end, ended) = tokio::sync::oneshot::channel();
    let completed = Arc::new(AtomicUsize::new(0));
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        call.start(&options(), vec![], deadline()).await.unwrap();
        conversation(
            (&link, &mut events, microphone),
            &mut call,
            session_options(),
            std::future::pending(),
            |notice| {
                if matches!(notice, SessionNotice::Completed(_)) {
                    completed.fetch_add(1, Ordering::SeqCst);
                }
                Ok(())
            },
        )
        .await
    };
    let remote = async {
        start(&mut server).await;
        reply(&mut server).await;
        ended.await.unwrap();
        drop(server);
    };
    let mut work = Box::pin(async { tokio::join!(client, remote).0 });
    pump(work.as_mut()).await;
    observer.finish(Ok(()));
    pump(work.as_mut()).await;
    assert_eq!(completed.load(Ordering::SeqCst), 1);
    end.send(()).unwrap();
    assert_eq!(work.await, Err(Error::Unavailable));
    assert_eq!(completed.load(Ordering::SeqCst), 1);
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn session_cancellation_during_reply_or_idle_stops_once_before_return() {
    for idle in [false, true] {
        let (mut io, mut server) = fixture();
        let (peer, state, _audio) = peer(true, None);
        let observer = Observer::default();
        let mut link = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = link.events().unwrap();
        wake(&observer);
        let microphone = microphone(&mut events).await;
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let completed = Arc::new(AtomicUsize::new(0));
        let client = async {
            let mut call = MediaCall::new(&mut io, peer);
            call.start(&options(), vec![], deadline()).await.unwrap();
            conversation(
                (&link, &mut events, microphone),
                &mut call,
                session_options(),
                async {
                    cancelled.await.unwrap();
                },
                |notice| {
                    if matches!(notice, SessionNotice::Completed(_)) {
                        completed.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(())
                },
            )
            .await
        };
        let remote = async {
            start(&mut server).await;
            if idle {
                reply(&mut server).await;
            }
            stop(&mut server).await;
        };
        let mut work = Box::pin(async { tokio::join!(client, remote).0 });
        pump(work.as_mut()).await;
        if idle {
            observer.finish(Ok(()));
            pump(work.as_mut()).await;
        }
        cancel.send(()).unwrap();
        if !idle {
            pump(work.as_mut()).await;
            observer.finish(Ok(())); // Acknowledged cut releases playback before stop.
        }
        assert_eq!(work.await, Err(Error::Control));
        assert_eq!(state.closes.load(Ordering::SeqCst), 1);
        assert_eq!(completed.load(Ordering::SeqCst), usize::from(idle));
        assert_eq!(
            observer
                .commands()
                .iter()
                .filter(|command| matches!(command, Command::Cut(_)))
                .count(),
            usize::from(!idle)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn voice_retained_wake_time_refresh_skips_muted_and_expired_wakes() {
    for muted in [false, true] {
        let (mut io, mut server) = fixture();
        let (peer, state, _audio) = peer(true, None);
        let observer = Observer::default();
        let mut link = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = link.events().unwrap();
        wake(&observer);
        let microphone = microphone(&mut events).await;
        let started = Arc::new(AtomicUsize::new(0));
        let client = async {
            let mut call = MediaCall::new(&mut io, peer);
            call.start(&options(), vec![], deadline()).await.unwrap();
            conversation(
                (&link, &mut events, microphone),
                &mut call,
                session_options(),
                std::future::pending(),
                |notice| {
                    if matches!(notice, SessionNotice::Started) {
                        started.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(())
                },
            )
            .await
        };
        let remote = async {
            start(&mut server).await;
            reply(&mut server).await;
            stop(&mut server).await;
        };
        let mut work = Box::pin(async { tokio::join!(client, remote).0 });
        pump(work.as_mut()).await;
        observer.finish(Ok(()));
        pump(work.as_mut()).await;
        if muted {
            observer.mute(true);
            wake(&observer);
            pump(work.as_mut()).await;
        }
        tokio::time::advance(Duration::from_secs(5)).await;
        wake(&observer);
        assert_eq!(work.await, Ok(1));
        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn session_observer_error_and_invalid_options_always_close() {
    for invalid in 0..7 {
        let (mut io, mut server) = fixture();
        let (peer, state, _audio) = peer(true, None);
        let observer = Observer::default();
        let mut link = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = link.events().unwrap();
        wake(&observer);
        let microphone = microphone(&mut events).await;
        let client = async {
            let mut call = MediaCall::new(&mut io, peer);
            call.start(&options(), vec![], deadline()).await.unwrap();
            let mut settings = session_options();
            match invalid {
                1 => settings.response_timeout = Duration::ZERO,
                2 => settings.played_timeout = Duration::ZERO,
                3 => settings.response_timeout = Duration::MAX,
                4 => settings.played_timeout = Duration::MAX,
                5 => settings.listen_timeout = Some(Duration::ZERO),
                6 => settings.listen_timeout = Some(Duration::MAX),
                _ => (),
            }
            conversation(
                (&link, &mut events, microphone),
                &mut call,
                settings,
                std::future::pending(),
                |_| Err(Error::Screen),
            )
            .await
        };
        let remote = async {
            start(&mut server).await;
            stop(&mut server).await;
        };
        let (result, ()) = tokio::join!(client, remote);
        assert_eq!(result, Err(if invalid == 0 { Error::Screen } else { Error::Input }));
        assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn session_stop_without_ack_is_bounded_and_cannot_report_success() {
    let (mut io, mut server) = fixture();
    let (peer, state, _audio) = peer(true, None);
    let (link, mut events, microphone, observer) = device().await;
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        call.start(&options(), vec![], deadline()).await.unwrap();
        let mut settings = session_options();
        settings.idle = Duration::ZERO;
        conversation(
            (&link, &mut events, microphone),
            &mut call,
            settings,
            std::future::pending(),
            |_| Ok(()),
        )
        .await
    };
    let remote = async {
        start(&mut server).await;
        reply(&mut server).await;
        let request = read(&mut server).await;
        assert_eq!(request["method"], "thread/realtime/stop");
        assert_eq!(request["params"]["threadId"], "thread-1");
        std::future::pending::<()>().await;
    };
    let mut work = Box::pin(async {
        tokio::select! { result = client => result, () = remote => unreachable!() }
    });
    pump(work.as_mut()).await;
    observer.finish(Ok(()));
    let start = Instant::now();
    assert_eq!(work.await, Err(Error::Deadline));
    assert_eq!(Instant::now() - start, Duration::from_secs(2));
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    assert_eq!(
        io.begin_request("account/read", &json!({})),
        Err(echo_codex::codex_rpc::RpcError::Disconnected)
    );
}
