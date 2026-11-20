//! T1: tool conversation drives runs, idle dispatch and cleanup on memory ports.
use super::*;
use crate::bridge_voice::{SessionNotice, SessionOptions, tool_conversation};
use echo_link::{
    EchoLink, LinkConnector, LinkEvent, LinkEvents, MicFrame,
    fake::{Command, FakeConnector, Observer},
};
use std::{pin::Pin, task::Poll, time::Duration};

fn settings() -> SessionOptions {
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

fn wake(observer: &Observer) {
    observer.wake(
        "Computer",
        vec![MicFrame {
            format: crate::bridge_audio::format(16_000),
            payload: vec![0; 640],
        }],
    );
}

async fn pump<F: Future>(mut work: Pin<&mut F>) {
    for _ in 0..128 {
        std::future::poll_fn(|cx| {
            assert!(work.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }
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
    let LinkEvent::Wake(run) = events.recv().await.unwrap().unwrap() else {
        unreachable!()
    };
    (link, events, run, observer)
}

async fn echo(server: &mut Server, id: &str, call: &str, turn_id: &str) {
    tool(server, json!(id), turn_id, call).await;
    assert_eq!(
        read(server).await,
        json!({"id":id,"result":{
            "contentItems":[{"type":"inputText","text":"{\"text\":\"echo\"}"}],"success":true
        }})
    );
}

async fn two_replies(
    server: &mut Server,
    starts: &mut mpsc::UnboundedReceiver<()>,
    completion: &mut mpsc::UnboundedReceiver<()>,
    observer: &Observer,
) {
    start(server).await;
    starts.recv().await.unwrap();
    turn(server, "turn/started", "turn-1").await;
    echo(server, "run-request", "call-1", "turn-1").await;
    done(server).await;
    // An off run retains admitted delegated work until its turn settles.
    turn(server, "turn/completed", "turn-1").await;
    completion.recv().await.unwrap();
    turn(server, "turn/started", "turn-2").await;
    echo(server, "idle-request", "call-2", "turn-2").await;
    turn(server, "turn/completed", "turn-2").await;
    wake(observer);
    ack_refresh(server).await;
    starts.recv().await.unwrap();
    done(server).await;
    completion.recv().await.unwrap();
    stop(server).await;
}

async fn ack_refresh(server: &mut Server) {
    let request = read(server).await;
    assert_eq!(request["method"], "thread/realtime/appendText");
    assert_eq!(request["params"]["threadId"], "thread-1");
    write(server, json!({"id":request["id"],"result":{}})).await;
}

fn assert_calls(host: &HostState) {
    assert_eq!(
        *host.calls.lock().unwrap(),
        vec![
            (
                "test.echo".into(),
                json!({"text":"echo"}),
                "8:thread-16:turn-16:call-1".into()
            ),
            (
                "test.echo".into(),
                json!({"text":"echo"}),
                "8:thread-16:turn-26:call-2".into()
            ),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn tools_conversation_dispatches_during_run_and_idle_then_reuses_call() {
    let (mut io, mut server) = fixture();
    let (peer, media, _audio) = peer(true, None);
    let (link, mut events, microphone, observer) = device().await;
    let host = Arc::new(HostState::default());
    let (completed, mut completion) = mpsc::unbounded_channel();
    let (started, mut starts) = mpsc::unbounded_channel();
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        call.start(&options(), catalog().tools().to_vec(), deadline())
            .await
            .unwrap();
        tool_conversation(
            (&link, &mut events, microphone),
            &mut call,
            (
                Host {
                    state: host.clone(),
                    stalled: false,
                    error: false,
                },
                catalog(),
                config(8),
                budget(),
            ),
            settings(),
            std::future::pending(),
            |notice| {
                if matches!(notice, SessionNotice::Started) {
                    started.send(()).unwrap();
                }
                if let SessionNotice::Completed(stats) = notice {
                    assert_eq!((stats.microphone_frames, stats.reply_sent), (1, 0));
                    completed.send(()).unwrap();
                }
                Ok(())
            },
        )
        .await
    };
    let remote = two_replies(&mut server, &mut starts, &mut completion, &observer);
    let mut work = Box::pin(async { tokio::join!(client, remote).0 });
    pump(work.as_mut()).await;
    observer.finish(Ok(()));
    pump(work.as_mut()).await;
    observer.finish(Ok(()));
    pump(work.as_mut()).await;
    tokio::time::advance(Duration::from_secs(5)).await;
    assert_eq!(work.await, Ok(2));
    assert_calls(&host);
    assert_eq!(media.offers.load(Ordering::SeqCst), 1);
    assert_eq!(media.closes.load(Ordering::SeqCst), 1);
    assert_eq!(*media.sent.lock().unwrap(), vec![[0; 320]; 2]);
    assert_eq!(observer.commands().iter().filter(|c| **c == Command::Stop).count(), 2);
    assert!(io.begin_request("account/read", &json!({})).is_ok());
}

#[tokio::test(start_paused = true)]
async fn tools_conversation_cancels_pending_effect_before_stop_without_result_replay() {
    let (mut io, mut server) = fixture();
    let (peer, media, _audio) = peer(true, None);
    let (link, mut events, microphone, observer) = device().await;
    let host = Arc::new(HostState::default());
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        call.start(&options(), catalog().tools().to_vec(), deadline())
            .await
            .unwrap();
        tool_conversation(
            (&link, &mut events, microphone),
            &mut call,
            (
                Host {
                    state: host.clone(),
                    stalled: true,
                    error: false,
                },
                catalog(),
                config(8),
                budget(),
            ),
            settings(),
            host.entered.notified(),
            |_| Ok(()),
        )
        .await
    };
    let remote = async {
        start(&mut server).await;
        turn(&mut server, "turn/started", "turn-1").await;
        tool(&mut server, json!("pending"), "turn-1", "call-1").await;
        // The next frame must be stop, never a fabricated success/result.
        stop(&mut server).await;
    };
    let mut work = Box::pin(async { tokio::join!(client, remote).0 });
    pump(work.as_mut()).await;
    assert!(observer.commands().iter().any(|c| matches!(c, Command::Cut(_))));
    observer.finish(Ok(()));
    assert_eq!(work.await, Err(Error::Control));
    assert_eq!(host.calls.lock().unwrap().len(), 1);
    assert_eq!(media.closes.load(Ordering::SeqCst), 1);
    assert!(io.begin_request("account/read", &json!({})).is_ok());
}

#[tokio::test(start_paused = true)]
async fn tools_conversation_invalid_dispatch_or_observer_still_stops_media() {
    for invalid in [false, true] {
        let (mut io, mut server) = fixture();
        let (peer, media, _audio) = peer(true, None);
        let (link, mut events, microphone, _) = device().await;
        let client = async {
            let mut call = MediaCall::new(&mut io, peer);
            call.start(&options(), catalog().tools().to_vec(), deadline())
                .await
                .unwrap();
            tool_conversation(
                (&link, &mut events, microphone),
                &mut call,
                (
                    Host {
                        state: Arc::default(),
                        stalled: false,
                        error: false,
                    },
                    catalog(),
                    config(if invalid { 0 } else { 8 }),
                    budget(),
                ),
                settings(),
                std::future::pending(),
                |_| Err(Error::Screen),
            )
            .await
        };
        let remote = async {
            start(&mut server).await;
            stop(&mut server).await;
        };
        assert_eq!(
            tokio::join!(client, remote).0,
            Err(if invalid { Error::Input } else { Error::Screen })
        );
        assert_eq!(media.closes.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn tools_daily_budget_survives_generations_interrupts_without_effect_and_resumes_next_day() {
    static DAY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(10);
    let budget = ToolBudget::new(1, || Ok(DAY.load(Ordering::SeqCst)));
    let state = Arc::new(HostState::default());
    for (day, early_tool, admitted, calls) in [
        (10, false, true, 1),
        (10, false, false, 1),
        (10, true, false, 1),
        (11, false, true, 2),
        (12, true, true, 3),
    ] {
        DAY.store(day, Ordering::SeqCst);
        assert_eq!(
            budget.check_call(),
            if admitted { Ok(()) } else { Err(Error::Unavailable) }
        );
        let (mut io, mut server) = fixture();
        let (peer, media, _tx) = peer(true, None);
        let client = async {
            let mut call = MediaCall::new(&mut io, peer);
            call.start(&options(), catalog().tools().to_vec(), deadline())
                .await
                .unwrap();
            let host = Host {
                state: state.clone(),
                stalled: false,
                error: false,
            };
            let mut session = ToolSession::new(&mut call, host, catalog(), config(8), budget.clone()).unwrap();
            finish(&mut session).await;
            drop(session);
            call.close(deadline()).await.unwrap();
        };
        let remote = daily_turn(&mut server, early_tool, admitted);
        tokio::join!(client, remote);
        assert_eq!(media.closes.load(Ordering::SeqCst), 1);
        assert_eq!(state.calls.lock().unwrap().len(), calls);
    }
}

async fn daily_turn(server: &mut Server, early_tool: bool, admitted: bool) {
    start(server).await;
    if !early_tool {
        turn(server, "turn/started", "turn-1").await;
        if !admitted {
            acknowledge_interrupt(server).await;
        }
    }
    tool(server, json!("daily"), "turn-1", "call-1").await;
    if early_tool {
        if !admitted {
            acknowledge_interrupt(server).await;
        }
        // The late matching notification must not consume admission twice.
        turn(server, "turn/started", "turn-1").await;
    }
    daily_result(server, admitted).await;
    turn(server, "turn/completed", "turn-1").await;
    done(server).await;
    stop(server).await;
}

async fn daily_result(server: &mut Server, admitted: bool) {
    let text = if admitted {
        "{\"text\":\"echo\"}"
    } else {
        "stale or duplicate tool request"
    };
    assert_eq!(
        read(server).await,
        json!({"id":"daily","result":{
            "contentItems":[{"type":"inputText","text":text}],"success":admitted
        }})
    );
    tool(server, json!("repeat"), "turn-1", "call-1").await;
    assert_eq!(
        read(server).await,
        json!({"id":"repeat","result":{
            "contentItems":[{"type":"inputText","text":"stale or duplicate tool request"}],"success":false
        }})
    );
}

// Observe lifecycle after pending work is settled, through the voice boundary.
pub(super) async fn lifecycle<R, W, P, H>(session: &mut ToolSession<'_, '_, R, W, P, H>) -> Vec<(bool, String)>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
    P: MediaPeer,
    H: ToolHost + 'static,
{
    let mut events = Vec::new();
    loop {
        match session.next().await.unwrap() {
            MediaEvent::Control(Event::TurnStarted { thread, turn }) => {
                assert_eq!(thread, "thread-1");
                events.push((true, turn));
            }
            MediaEvent::Control(Event::TurnCompleted { thread, turn, .. }) => {
                assert_eq!(thread, "thread-1");
                events.push((false, turn));
            }
            MediaEvent::Control(Event::ReplyDone { text }) => {
                assert_eq!(text, "done");
                return events;
            }
            MediaEvent::Control(Event::Other) => (),
            _ => unreachable!("unexpected lifecycle event"),
        }
    }
}
