//! T1: delegated lifecycle and cancellation-safe interrupt delivery on JSONL.
use super::{call_tests::*, *};
use std::{future::Future, task::Poll, time::Duration};

async fn pending<T>(future: std::pin::Pin<&mut impl Future<Output = T>>) {
    let mut future = future;
    std::future::poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
}

mod trace_tests;

#[tokio::test(start_paused = true)]
async fn call_turn_events_match_thread_and_keep_early_lifecycle_order() {
    let (mut io, mut server) = fixture();
    let client = async {
        let mut call = Call::new(&mut io);
        call.start(&options(), vec![], "fixture-offer", deadline())
            .await
            .unwrap();
        assert_eq!(call.thread_id(), Some("thread-1"));
        assert!(
            matches!(call.next().await.unwrap(), Event::TurnStarted {thread,turn} if thread=="thread-1" && turn=="turn-1")
        );
        assert!(
            matches!(call.next().await.unwrap(), Event::TurnCompleted {thread,turn} if thread=="thread-1" && turn=="turn-1")
        );
        call.stop(deadline()).await.unwrap();
    };
    let remote = async {
        thread(&mut server).await;
        let request = read(&mut server).await;
        write(&mut server, json!({"id":request["id"],"result":{}})).await;
        for (method, thread) in [
            ("turn/started", "foreign"),
            ("turn/started", "thread-1"),
            ("turn/completed", "thread-1"),
        ] {
            notification(
                &mut server,
                method,
                json!({"threadId":thread,"turn":{"id":"turn-1","status":"inProgress"}}),
            )
            .await;
        }
        notification(
            &mut server,
            "thread/realtime/sdp",
            json!({"threadId":"thread-1","sdp":"fixture-answer"}),
        )
        .await;
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
}

#[tokio::test(start_paused = true)]
async fn call_malformed_turn_id_fails_closed_without_payload_diagnostics() {
    for id in [json!(null), json!(""), json!("bad\nid"), json!("x".repeat(257))] {
        let (mut io, mut server) = fixture();
        let client = async {
            let mut call = Call::new(&mut io);
            call.start(&options(), vec![], "fixture-offer", deadline())
                .await
                .unwrap();
            assert!(matches!(call.next().await, Err(Error::Decode)));
        };
        let remote = async {
            start(&mut server).await;
            notification(
                &mut server,
                "turn/started",
                json!({"threadId":"thread-1","turn":{"id":id}}),
            )
            .await;
        };
        tokio::join!(client, remote);
        assert_eq!(
            io.begin_request("account/read", &json!({})),
            Err(RpcError::Disconnected)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn call_interrupt_preserves_id_across_cancelled_polls_and_buffers_notifications() {
    let (mut io, mut server) = fixture();
    let ready = tokio::sync::Notify::new();
    let released = tokio::sync::Notify::new();
    let client = async {
        let mut call = Call::new(&mut io);
        let params = TurnInterrupt {
            thread_id: "thread-1".into(),
            turn_id: "turn-1".into(),
        };
        assert_eq!(call.interrupt(&params, deadline()), Err(Error::Input));
        call.start(&options(), vec![], "fixture-offer", deadline())
            .await
            .unwrap();
        assert_eq!(
            call.interrupt(
                &TurnInterrupt {
                    thread_id: "foreign".into(),
                    turn_id: "turn-1".into()
                },
                deadline()
            ),
            Err(Error::Input)
        );
        assert_eq!(call.interrupt(&params, Instant::now()), Err(Error::Deadline));
        call.interrupt(&params, deadline()).unwrap();
        assert_eq!(call.interrupt(&params, deadline()), Err(Error::Input));
        {
            let mut next = Box::pin(call.next());
            pending(next.as_mut()).await;
            ready.notified().await;
            pending(next.as_mut()).await;
        }
        // Cancel only the wait, not the admitted request or partial output.
        released.notify_one();
        assert!(matches!(call.next().await.unwrap(), Event::TurnCompleted {turn,..} if turn=="turn-1"));
        call.stop(deadline()).await.unwrap();
    };
    let remote = async {
        start(&mut server).await;
        let request = read(&mut server).await;
        assert_eq!(
            request,
            json!({"id":3,"method":"turn/interrupt","params":{"threadId":"thread-1","turnId":"turn-1"}})
        );
        notification(
            &mut server,
            "turn/completed",
            json!({"threadId":"thread-1","turn":{"id":"turn-1"}}),
        )
        .await;
        ready.notify_one();
        released.notified().await;
        write(&mut server, json!({"id":request["id"],"result":{}})).await;
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
    assert!(io.begin_request("account/read", &json!({})).is_ok());
}

#[tokio::test(start_paused = true)]
async fn call_interrupt_deadline_and_remote_failure_close_the_generation() {
    for remote_failure in [false, true] {
        let (mut io, mut server) = fixture();
        let client = async {
            let mut call = Call::new(&mut io);
            call.start(&options(), vec![], "fixture-offer", deadline())
                .await
                .unwrap();
            call.interrupt(
                &TurnInterrupt {
                    thread_id: "thread-1".into(),
                    turn_id: "turn-1".into(),
                },
                deadline(),
            )
            .unwrap();
            assert!(
                matches!(call.next().await, Err(error) if error == if remote_failure {Error::Unavailable} else {Error::Deadline})
            );
        };
        let remote = async {
            start(&mut server).await;
            let request = read(&mut server).await;
            if remote_failure {
                write(
                    &mut server,
                    json!({"id":request["id"],"error":{"code":-32000,"message":"untrusted payload"}}),
                )
                .await;
            } else {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        };
        tokio::join!(client, remote);
        assert_eq!(
            io.begin_request("account/read", &json!({})),
            Err(RpcError::Disconnected)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn call_projects_only_matching_speech_items_and_ignores_unknown_items() {
    let (mut io, mut server) = fixture();
    let client = async {
        let mut call = Call::new(&mut io);
        call.start(&options(), vec![], "fixture-offer", deadline())
            .await
            .unwrap();
        for expected in [false, false, true] {
            let event = call.next().await.unwrap();
            assert_eq!(matches!(event, Event::SpeechStarted), expected);
            if !expected {
                assert!(matches!(event, Event::Other));
            }
        }
        call.stop(deadline()).await.unwrap();
    };
    let remote = async {
        start(&mut server).await;
        for (thread, kind) in [
            ("foreign", "input_audio_buffer.speech_started"),
            ("thread-1", "future.item"),
            ("thread-1", "input_audio_buffer.speech_started"),
        ] {
            notification(
                &mut server,
                "thread/realtime/itemAdded",
                json!({"threadId":thread,"item":{"type":kind}}),
            )
            .await;
        }
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
}

#[tokio::test(start_paused = true)]
async fn voice_listen_goodbye_projects_only_exact_current_thread_handoff_once() {
    let (mut io, mut server) = fixture();
    let client = async {
        let mut call = Call::new(&mut io);
        call.start(&options(), vec![], "fixture-offer", deadline())
            .await
            .unwrap();
        for expected in [false, false, true, false] {
            let event = call.next().await.unwrap();
            assert_eq!(matches!(event, Event::GoodbyeHandoff), expected);
            if !expected {
                assert!(matches!(event, Event::Other));
            }
        }
        call.stop(deadline()).await.unwrap();
    };
    let remote = async {
        start(&mut server).await;
        for (thread, text) in [
            ("foreign", "end the session"),
            ("thread-1", "End the session"),
            ("thread-1", "end the session"),
            ("thread-1", "end the session"),
        ] {
            notification(&mut server, "thread/realtime/itemAdded",
                json!({"threadId":thread,"item":{"type":"handoff_request","handoff_id":"synthetic","input_transcript":text}})).await;
        }
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
}
