//! T1: single-reader tool scheduling with memory JSONL, controlled PCM and time.
use super::*;
use crate::{
    bridge_codex::{ToolBudget, ToolSession},
    bridge_voice::VoicePort,
};
use echo_agent_api::{ToolError, ToolHost, ToolResult, ToolSpec};
use echo_delegation::codex_dispatch::Catalog;
use serde_json::Value;
use std::future::Future;
use tokio::sync::Notify;

struct Host {
    state: Arc<HostState>,
    stalled: bool,
    error: bool,
}
#[derive(Default)]
struct HostState {
    calls: Mutex<Vec<(String, Value, String)>>,
    entered: Notify,
}
impl ToolHost for Host {
    fn list_tools(&self) -> impl Future<Output = Result<Vec<ToolSpec>, ToolError>> {
        std::future::ready(Ok(vec![]))
    }
    async fn call_tool(&self, name: &str, args: Value, key: &str) -> Result<ToolResult, ToolError> {
        self.state
            .calls
            .lock()
            .unwrap()
            .push((name.into(), args.clone(), key.into()));
        self.state.entered.notify_one();
        if self.stalled {
            std::future::pending::<()>().await;
        }
        if self.error {
            return Err(ToolError::Server("untrusted body".into()));
        }
        Ok(ToolResult {
            content: args,
            is_error: false,
        })
    }
}
fn catalog() -> Catalog {
    Catalog::new(
        vec![ToolSpec {
            name: "test.echo".into(),
            description: "Echo text".into(),
            input_schema: json!({"type":"object"}),
        }],
        &["test.echo".into()],
    )
    .unwrap()
}
fn budget() -> ToolBudget {
    ToolBudget::new(100, || Ok(10))
}
async fn turn(server: &mut Server, method: &str, id: &str) {
    notification(server, method, json!({"threadId":"thread-1","turn":{"id":id}})).await;
}
async fn tool(server: &mut Server, request: Value, turn: &str, call: &str) {
    write(
        server,
        json!({"id":request,"method":"item/tool/call","params":{
            "threadId":"thread-1","turnId":turn,"callId":call,"tool":"test_echo","arguments":{"text":"echo"}
        }}),
    )
    .await;
}
async fn done(server: &mut Server) {
    notification(
        server,
        "thread/realtime/transcript/done",
        json!({"threadId":"thread-1","role":"assistant","text":"done"}),
    )
    .await;
}
async fn acknowledge_interrupt(server: &mut Server) {
    let interrupt = read(server).await;
    assert_eq!(interrupt["method"], "turn/interrupt");
    assert_eq!(interrupt["params"], json!({"threadId":"thread-1","turnId":"turn-1"}));
    write(server, json!({"id":interrupt["id"],"result":{}})).await;
}

async fn finish<R, W, P, H>(session: &mut ToolSession<'_, '_, R, W, P, H>)
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
    P: MediaPeer,
    H: ToolHost + 'static,
{
    loop {
        match session.next().await.unwrap().into_parts() {
            (Some(Event::ReplyDone { text }), _) => {
                assert_eq!(text, "done");
                return;
            }
            (Some(Event::Other | Event::TurnStarted { .. } | Event::TurnCompleted { .. }), _) => (),
            _ => unreachable!("unexpected session event"),
        }
    }
}

#[tokio::test(start_paused = true)]
async fn tools_correlate_original_ids_no_replay_and_sanitize_host_failure() {
    for error in [false, true] {
        let (mut io, mut server) = fixture();
        let (peer, _, _tx) = peer(true, None);
        let state = Arc::new(HostState::default());
        let client = async {
            let mut call = MediaCall::new(&mut io, peer);
            call.start(&options(), catalog().tools().to_vec(), deadline())
                .await
                .unwrap();
            let host = Host {
                state: state.clone(),
                stalled: false,
                error,
            };
            let mut session = ToolSession::new(&mut call, host, catalog(), budget()).unwrap();
            finish(&mut session).await;
            drop(session);
            call.close(deadline()).await.unwrap();
        };
        let remote = async {
            let settings = start(&mut server).await;
            assert_eq!(
                settings["dynamicTools"],
                serde_json::to_value(catalog().tools()).unwrap()
            );
            turn(&mut server, "turn/started", "turn-1").await;
            tool(&mut server, json!("original"), "turn-1", "call-1").await;
            let text = if error {
                "tool execution failed".to_owned()
            } else {
                json!({"text":"echo"}).to_string()
            };
            assert_eq!(
                read(&mut server).await,
                json!({"id":"original","result":{"contentItems":[{"type":"inputText","text":text}],"success":!error}})
            );
            for (id, turn) in [(json!(42), "turn-1"), (json!("stale"), "old-turn")] {
                tool(&mut server, id.clone(), turn, "call-1").await;
                assert_eq!(
                    read(&mut server).await,
                    json!({"id":id,"result":{"contentItems":[{"type":"inputText","text":"stale or duplicate tool request"}],"success":false}})
                );
            }
            turn(&mut server, "turn/completed", "turn-1").await;
            done(&mut server).await;
            stop(&mut server).await;
        };
        tokio::join!(client, remote);
        assert_eq!(
            *state.calls.lock().unwrap(),
            vec![(
                "test.echo".into(),
                json!({"text":"echo"}),
                "8:thread-16:turn-16:call-1".into()
            )]
        );
    }
}

#[tokio::test(start_paused = true)]
async fn tools_pending_request_does_not_block_pcm_or_transcript_before_cancellation() {
    let (mut io, mut server) = fixture();
    let (peer, state, tx) = peer(true, None);
    let host_state = Arc::new(HostState::default());
    let pcm_sent = Notify::new();
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        call.start(&options(), catalog().tools().to_vec(), deadline())
            .await
            .unwrap();
        let host = Host {
            state: host_state.clone(),
            stalled: true,
            error: false,
        };
        let mut session = ToolSession::new(&mut call, host, catalog(), budget()).unwrap();
        let mut transcripts = Vec::new();
        loop {
            match session.next().await.unwrap().into_parts() {
                (None, Some(frame)) => {
                    assert_eq!(frame, [17; 320]);
                    session.send([23; 320]).unwrap();
                    pcm_sent.notify_one();
                }
                (Some(Event::Transcript { text, .. }), _) => {
                    transcripts.push(text);
                }
                (Some(Event::ReplyDone { .. }), _) => break,
                (Some(Event::Other | Event::TurnStarted { .. } | Event::TurnCompleted { .. }), _) => (),
                (Some(_), _) | (None, None) => unreachable!("unexpected event"),
            }
        }
        assert_eq!(transcripts, ["pending tool"]);
        drop(session);
        call.close(deadline()).await.unwrap();
    };
    let remote = async {
        start(&mut server).await;
        turn(&mut server, "turn/started", "turn-1").await;
        tool(&mut server, json!("slow"), "turn-1", "call-1").await;
        host_state.entered.notified().await;
        tx.send([17; 320]).await.unwrap();
        notification(
            &mut server,
            "thread/realtime/transcript/delta",
            json!({"threadId":"thread-1","role":"assistant","delta":"pending tool"}),
        )
        .await;
        pcm_sent.notified().await;
        turn(&mut server, "turn/completed", "turn-1").await;
        let failure = read(&mut server).await;
        assert_eq!(
            failure,
            json!({"id":"slow","result":{"contentItems":[{"type":"inputText","text":"tool request cancelled"}],"success":false}})
        );
        done(&mut server).await;
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
    assert_eq!(state.sent.lock().unwrap().as_slice(), &[[23; 320]]);
    assert_eq!(host_state.calls.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn tools_completed_turn_cancels_pending_and_queued_work_before_new_turn() {
    let (mut io, mut server) = fixture();
    let (peer, _, _tx) = peer(true, None);
    let state = Arc::new(HostState::default());
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        call.start(&options(), catalog().tools().to_vec(), deadline())
            .await
            .unwrap();
        let host = Host {
            state: state.clone(),
            stalled: true,
            error: false,
        };
        let mut session = ToolSession::new(&mut call, host, catalog(), budget()).unwrap();
        assert_eq!(
            conversation::lifecycle(&mut session).await,
            [
                (true, "turn-1".into()),
                (false, "turn-1".into()),
                (true, "turn-2".into()),
                (false, "turn-2".into())
            ]
        );
        drop(session);
        call.close(deadline()).await.unwrap();
    };
    let remote = async {
        start(&mut server).await;
        turn(&mut server, "turn/started", "turn-1").await;
        tool(&mut server, json!("pending"), "turn-1", "call-1").await;
        state.entered.notified().await;
        tool(&mut server, json!("queued"), "turn-1", "call-2").await;
        turn(&mut server, "turn/completed", "turn-1").await;
        turn(&mut server, "turn/started", "turn-2").await;
        assert_eq!(
            read(&mut server).await,
            json!({"id":"pending","result":{"contentItems":[{"type":"inputText","text":"tool request cancelled"}],"success":false}})
        );
        assert_eq!(
            read(&mut server).await,
            json!({"id":"queued","result":{"contentItems":[{"type":"inputText","text":"stale or duplicate tool request"}],"success":false}})
        );
        turn(&mut server, "turn/completed", "old-turn").await;
        turn(&mut server, "turn/completed", "turn-2").await;
        done(&mut server).await;
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
    assert_eq!(state.calls.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn tools_queued_distinct_calls_execute_once_and_call_ids_are_scoped_to_turn() {
    let (mut io, mut server) = fixture();
    let (peer, _, _tx) = peer(true, None);
    let state = Arc::new(HostState::default());
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
        let mut session = ToolSession::new(&mut call, host, catalog(), budget()).unwrap();
        finish(&mut session).await;
        drop(session);
        call.close(deadline()).await.unwrap();
    };
    let remote = async {
        start(&mut server).await;
        turn(&mut server, "turn/started", "turn-1").await;
        for index in 1..=2 {
            tool(&mut server, json!(index), "turn-1", &format!("call-{index}")).await;
        }
        for index in 1..=2 {
            assert_eq!(
                read(&mut server).await,
                json!({"id":index,"result":{"contentItems":[{"type":"inputText","text":"{\"text\":\"echo\"}"}],"success":true}})
            );
        }
        turn(&mut server, "turn/completed", "turn-1").await;
        turn(&mut server, "turn/started", "turn-2").await;
        tool(&mut server, json!(3), "turn-2", "call-1").await;
        assert_eq!(read(&mut server).await["result"]["success"], true);
        turn(&mut server, "turn/completed", "turn-2").await;
        done(&mut server).await;
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
    let calls = state.calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls.iter().map(|(_, _, key)| key.as_str()).collect::<Vec<_>>(),
        [
            "8:thread-16:turn-16:call-1",
            "8:thread-16:turn-16:call-2",
            "8:thread-16:turn-26:call-1"
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn tools_pending_queue_overflow_fails_and_cleanup_reaps_media_without_extra_effect() {
    let (mut io, mut server) = fixture();
    let (peer, media, _tx) = peer(true, None);
    let state = Arc::new(HostState::default());
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        call.start(&options(), catalog().tools().to_vec(), deadline())
            .await
            .unwrap();
        let host = Host {
            state: state.clone(),
            stalled: true,
            error: false,
        };
        let mut session = ToolSession::new(&mut call, host, catalog(), budget()).unwrap();
        loop {
            match session.next().await.map(|event| event.into_parts().0) {
                Ok(Some(Event::Other | Event::TurnStarted { .. } | Event::TurnCompleted { .. })) => (),
                Err(error) => {
                    assert_eq!(error, Error::Unavailable);
                    break;
                }
                _ => unreachable!(),
            }
        }
        drop(session);
        call.close(deadline()).await.unwrap();
    };
    let remote = async {
        start(&mut server).await;
        turn(&mut server, "turn/started", "turn-1").await;
        tool(&mut server, json!(0), "turn-1", "call-0").await;
        state.entered.notified().await;
        for index in 1..=33 {
            tool(&mut server, json!(index), "turn-1", &format!("call-{index}")).await;
        }
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
    assert_eq!(state.calls.lock().unwrap().len(), 1);
    assert_eq!(media.closes.load(Ordering::SeqCst), 1);
}

#[path = "voice_tool_session_cases.rs"]
mod conversation;
