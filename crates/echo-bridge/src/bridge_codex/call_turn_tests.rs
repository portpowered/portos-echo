//! T1: delegated lifecycle and cancellation-safe interrupt delivery on JSONL.
use super::{call_tests::*, *};
use serde_json::Value;
use std::io::Write as _;
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
            matches!(call.next().await.unwrap(), Event::TurnCompleted {thread,turn,answered:false} if thread=="thread-1" && turn=="turn-1")
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

/// A missed acknowledgement closes the generation. A remote refusal does not:
/// the turn had already ended, which is all the interrupt asked for.
#[tokio::test(start_paused = true)]
async fn call_interrupt_deadline_closes_the_generation_and_remote_refusal_does_not() {
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
            if remote_failure {
                assert!(matches!(call.next().await, Ok(Event::SpeechStarted)));
                call.stop(deadline()).await.unwrap();
            } else {
                assert_eq!(call.next().await.err(), Some(Error::Deadline));
            }
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
                notification(
                    &mut server,
                    "thread/realtime/itemAdded",
                    json!({"threadId":"thread-1","item":{"type":"input_audio_buffer.speech_started"}}),
                )
                .await;
                stop(&mut server).await;
            } else {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        };
        tokio::join!(client, remote);
        if !remote_failure {
            assert_eq!(
                io.begin_request("account/read", &json!({})),
                Err(RpcError::Disconnected)
            );
        }
    }
}

/// An applied end's interrupt is best effort: a missed acknowledgement
/// cancels it and the call goes on, retained for the next wake.
#[tokio::test(start_paused = true)]
async fn call_interrupt_after_end_survives_a_missed_acknowledgement() {
    let (mut io, mut server) = fixture();
    let client = async {
        let mut call = Call::new(&mut io);
        call.start(&options(), vec![], "fixture-offer", deadline())
            .await
            .unwrap();
        let params = TurnInterrupt {
            thread_id: "thread-1".into(),
            turn_id: "turn-1".into(),
        };
        let ack = Instant::now() + Duration::from_secs(2);
        call.interrupt_after_end(&params, ack).unwrap();
        assert!(matches!(call.next().await, Ok(Event::SpeechStarted)));
        call.stop(deadline()).await.unwrap();
    };
    let remote = async {
        start(&mut server).await;
        assert_eq!(read(&mut server).await["method"], "turn/interrupt");
        tokio::time::sleep(Duration::from_secs(5)).await;
        notification(
            &mut server,
            "thread/realtime/itemAdded",
            json!({"threadId":"thread-1","item":{"type":"input_audio_buffer.speech_started"}}),
        )
        .await;
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
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

/// The real hand-off carries the user's words and the transcript since the
/// last hand-off. No field names the voice model's intent, so every hand-off
/// projects alike and only the runner's final text marks a turn as answered.
#[tokio::test(start_paused = true)]
async fn voice_listen_projects_each_current_thread_handoff_once_whatever_the_user_said() {
    let (mut io, mut server) = fixture();
    let client = async {
        let mut call = Call::new(&mut io);
        call.start(&options(), vec![], "fixture-offer", deadline())
            .await
            .unwrap();
        for answered in [false, true, false] {
            assert!(matches!(call.next().await.unwrap(), Event::Other));
            assert!(matches!(call.next().await.unwrap(), Event::ReplyHandoff(_)));
            assert!(matches!(call.next().await.unwrap(), Event::Other));
            assert!(matches!(call.next().await.unwrap(), Event::TurnStarted { .. }));
            for _ in 0..3 {
                assert!(matches!(call.next().await.unwrap(), Event::Other));
            }
            assert!(
                matches!(call.next().await.unwrap(), Event::TurnCompleted { answered: seen, .. } if seen == answered)
            );
        }
        call.stop(deadline()).await.unwrap();
    };
    let remote = async {
        start(&mut server).await;
        for (id, words, text) in [
            ("farewell", "okay thanks, bye", ""),
            ("ordinary", "thanks, and what time is it?", "It is noon."),
            ("magic", "end the session", " "),
        ] {
            let item = json!({"type":"handoff_request","handoff_id":id,"item_id":id,"input_transcript":words,
                "active_transcript":[{"role":"user","text":words},{"role":"assistant","text":"Goodbye."}]});
            let turn = json!({"threadId":"thread-1","turn":{"id":id,"status":"completed"}});
            let agent = |thread: &str, phase: serde_json::Value, text: &str| json!({"threadId":thread,"turnId":id,"item":{"type":"agentMessage","phase":phase,"text":text}});
            let foreign = json!({"threadId":"foreign","item":item});
            notification(&mut server, "thread/realtime/itemAdded", foreign).await;
            for _ in 0..2 {
                let added = json!({"threadId":"thread-1","item":item});
                notification(&mut server, "thread/realtime/itemAdded", added).await;
            }
            notification(&mut server, "turn/started", turn.clone()).await;
            for (thread, phase, said) in [
                ("thread-1", json!("commentary"), "Working."),
                ("foreign", serde_json::Value::Null, "Foreign."),
                ("thread-1", json!("final_answer"), text),
            ] {
                notification(&mut server, "item/completed", agent(thread, phase, said)).await;
            }
            notification(&mut server, "turn/completed", turn).await;
        }
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
}

#[tokio::test(start_paused = true)]
async fn runner_fast_mode_does_not_change_live_request() {
    let ((mut standard, live_standard), (mut fast, live_fast)) =
        tokio::join!(fast_mode_requests(false), fast_mode_requests(true));
    assert_eq!(
        standard["params"].as_object_mut().unwrap().remove("serviceTier"),
        Some(Value::Null)
    );
    assert_eq!(
        fast["params"].as_object_mut().unwrap().remove("serviceTier"),
        Some(json!("priority"))
    );
    assert_eq!(standard, fast);
    assert_eq!(live_standard, live_fast);
    assert_eq!(live_fast["method"], "thread/realtime/start");
    writeln!(
        std::io::stdout(),
        "Fixed-input signaling: runner priority versus present null, complete Live request equal"
    )
    .unwrap();
}

async fn fast_mode_requests(fast: bool) -> (Value, Value) {
    let (mut io, mut server) = fixture();
    let client = async {
        let options = Options {
            fast,
            effort: Some("low"),
            ..options()
        };
        Call::new(&mut io)
            .start(&options, vec![], "fixture-offer", deadline())
            .await
            .unwrap()
    };
    let peer = async {
        let runner = read(&mut server).await;
        assert_eq!(runner["method"], "thread/start");
        write(
            &mut server,
            json!({"id":runner["id"],"result":{"thread":{"id":"fixture-thread"}}}),
        )
        .await;
        let live = read(&mut server).await;
        write(&mut server, json!({"id":live["id"],"result":{}})).await;
        notification(
            &mut server,
            "thread/realtime/sdp",
            json!({"threadId":"fixture-thread","sdp":"fixture-answer"}),
        )
        .await;
        (runner, live)
    };
    let (answer, requests) = tokio::join!(client, peer);
    assert_eq!(answer, "fixture-answer");
    requests
}
