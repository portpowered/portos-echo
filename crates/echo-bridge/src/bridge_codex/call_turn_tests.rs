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

type TracedCall<'a> =
    Call<'a, tokio::io::ReadHalf<tokio::io::DuplexStream>, tokio::io::WriteHalf<tokio::io::DuplexStream>>;

fn traced<'a>(io: &'a mut Io, trace: crate::bridge_trace::Trace) -> TracedCall<'a> {
    let mut call = Call::new(io);
    call.thread = Some("thread-1".into());
    call.phase = Phase::Active;
    let catalog = echo_delegation::codex_dispatch::Catalog::new(vec![], &[]).unwrap();
    call.tool_trace = Some(crate::bridge_trace::ToolTrace::new(trace, &catalog));
    call
}

fn project(call: &mut TracedCall<'_>, method: &str, params: Value) -> Result<Event, Error> {
    let raw = serde_json::to_vec(&json!({"method":method,"params":params})).unwrap();
    call.project(echo_codex::codex_protocol::decode(&raw).unwrap())
}

fn handoff(id: &str) -> Value {
    json!({"threadId":"thread-1","item":{"type":"handoff_request","handoff_id":id,
        "item_id":"item-1","input_transcript":"synthetic-query-7","active_transcript":[]}})
}

fn trace_rows(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn agent_answers(call: &mut TracedCall<'_>) {
    project(
        call,
        "item/completed",
        json!({"threadId":"foreign","turnId":"turn-1",
        "item":{"type":"agentMessage","text":"foreign answer"}}),
    )
    .unwrap();
    project(
        call,
        "item/completed",
        json!({"threadId":"thread-1","turnId":"turn-1",
        "item":{"type":"agentMessage","phase":"commentary","text":"not final"}}),
    )
    .unwrap();
    project(
        call,
        "item/completed",
        json!({"threadId":"thread-1","turnId":"turn-1",
        "item":{"type":"agentMessage","phase":"final_answer","text":"synthetic-result-7"}}),
    )
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn delegation_trace_native_final_agent_and_completed_turn_never_claim_append() {
    for payloads in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let trace = crate::bridge_trace::Trace::open(&path, "api_key")
            .unwrap()
            .with_payloads(payloads);
        trace.tool_run(Some("run-7")).unwrap();
        let (mut io, _server) = fixture();
        let mut call = traced(&mut io, trace.clone());
        project(&mut call, "thread/realtime/itemAdded", handoff("delegation-7")).unwrap();
        project(&mut call, "thread/realtime/itemAdded", handoff("delegation-7")).unwrap();
        project(
            &mut call,
            "turn/started",
            json!({"threadId":"thread-1","turn":{"id":"turn-1"}}),
        )
        .unwrap();
        agent_answers(&mut call);
        tokio::time::advance(Duration::from_millis(23)).await;
        trace.tool_run(Some("different-run")).unwrap();
        for _ in 0..2 {
            project(
                &mut call,
                "turn/completed",
                json!({"threadId":"thread-1","turn":{"id":"turn-1","status":"completed"}}),
            )
            .unwrap();
        }
        call.fail();
        drop(call);
        let rows = trace_rows(&path);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["delegation_id"], "delegation-7");
        assert_eq!(rows[1]["run_id"], "run-7");
        assert_eq!(rows[1]["turn_id"], "turn-1");
        assert_eq!(rows[1]["thread_id"], "thread-1");
        assert_eq!(rows[1]["outcome"], "completed");
        assert_eq!(rows[1]["handback"], "unobserved");
        assert_eq!(rows[1]["duration_ms"], 23);
        assert_eq!(rows[1]["tool_counts"], json!({}));
        assert_eq!(
            rows[0].get("task"),
            payloads.then(|| handoff("delegation-7")["item"].clone()).as_ref()
        );
        assert_eq!(
            rows[1].get("answer"),
            payloads.then(|| json!({"text":"synthetic-result-7"})).as_ref()
        );
        for row in rows {
            writeln!(std::io::stdout(), "{row}").unwrap();
        }
    }
}

fn terminal_case<'a>(call: &mut TracedCall<'_>, case: &'a str) -> (&'a str, Option<&'static str>) {
    match case {
        "failed" | "interrupted" | "timed_out" => {
            project(
                call,
                "turn/completed",
                json!({"threadId":"thread-1","turn":{"id":"turn-1",
            "status":if case == "failed" { "failed" } else { "interrupted" }}}),
            )
            .unwrap();
            (case, (case == "failed").then_some("turn_failed"))
        }
        "realtime_error" => {
            assert!(matches!(
                project(
                    call,
                    "thread/realtime/error",
                    json!({"threadId":"thread-1","message":"untrusted error"})
                ),
                Err(Error::Unavailable)
            ));
            ("failed", Some("realtime_error"))
        }
        "closed" => {
            project(call, "thread/realtime/closed", json!({"threadId":"thread-1"})).unwrap();
            ("cancelled", None)
        }
        "malformed" => {
            assert!(matches!(
                project(
                    call,
                    "turn/started",
                    json!({"threadId":"thread-1","turn":{"id":"bad\nid"}})
                ),
                Err(Error::Decode)
            ));
            call.fail();
            ("failed", Some("control_failed"))
        }
        _ => ("cancelled", None),
    }
}

#[tokio::test(start_paused = true)]
async fn delegation_trace_native_terminal_causes_and_cleanup_are_exactly_once() {
    for case in [
        "failed",
        "interrupted",
        "timed_out",
        "realtime_error",
        "closed",
        "drop",
        "malformed",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let trace = crate::bridge_trace::Trace::open(&path, "api_key").unwrap();
        let (mut io, _server) = fixture();
        let mut call = traced(&mut io, trace);
        project(
            &mut call,
            "turn/started",
            json!({"threadId":"thread-1","turn":{"id":"turn-1"}}),
        )
        .unwrap();
        project(&mut call, "thread/realtime/itemAdded", handoff("delegation-7")).unwrap();
        if case == "timed_out" {
            call.tool_trace.as_mut().unwrap().timeout("turn-1");
        }
        let (outcome, reason) = terminal_case(&mut call, case);
        drop(call);
        let rows = trace_rows(&path);
        assert_eq!(rows.len(), 2, "{case}");
        assert_eq!(rows[1]["outcome"], outcome, "{case}");
        assert_eq!(rows[1]["handback"], "unobserved");
        assert_eq!(rows[1].get("reason_class").and_then(Value::as_str), reason);
        assert!(rows[1].get("answer").is_none());
        writeln!(std::io::stdout(), "{}", rows[1]).unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn delegation_trace_native_child_eof_keeps_app_server_exit_after_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let trace = crate::bridge_trace::Trace::open(&path, "api_key").unwrap();
    let (mut io, server) = fixture();
    let mut call = traced(&mut io, trace);
    project(&mut call, "thread/realtime/itemAdded", handoff("delegation-7")).unwrap();
    drop(server);
    assert!(matches!(call.next().await, Err(Error::Unavailable)));
    call.stop(deadline()).await.unwrap();
    drop(call);
    let rows = trace_rows(&path);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1]["outcome"], "failed");
    assert_eq!(rows[1]["reason_class"], "app_server_exit");
    assert!(rows[1].get("answer").is_none());
    assert!(rows[1].get("turn_id").is_none());
    writeln!(std::io::stdout(), "{}", rows[1]).unwrap();
}

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
