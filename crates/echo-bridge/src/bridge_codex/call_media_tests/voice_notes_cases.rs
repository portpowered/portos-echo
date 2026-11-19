//! T3 component exception: production admission/catalog/dispatcher/router/files,
//! memory JSONL app-server and controlled media, no remote service or hardware.
use super::*;
use crate::{
    bridge_codex::{ToolBudget, ToolSession},
    bridge_notes::{Local, NAMES, Notes},
    bridge_trace::Trace,
};
use echo_agent_api::{ToolError, ToolHost, ToolResult, ToolSpec};
use echo_config::{AgentConfig, AgentTools, HomeConfig};
use echo_delegation::codex_dispatch::Catalog;
use serde_json::Value;
use std::time::Duration;

#[path = "voice_notes_matrix.rs"]
mod matrix;

#[derive(Clone)]
struct NoMcp;
impl ToolHost for NoMcp {
    fn list_tools(&self) -> impl Future<Output = Result<Vec<ToolSpec>, ToolError>> {
        std::future::ready(Ok(vec![]))
    }
    fn call_tool(&self, _: &str, _: Value, _: &str) -> impl Future<Output = Result<ToolResult, ToolError>> {
        std::future::poll_fn(|_| unreachable!("local calls must never reach MCP"))
    }
}

fn catalog(notes: &Notes) -> Catalog {
    Catalog::with_local(vec![], &[], notes.specs(), &NAMES).unwrap()
}
fn config() -> AgentConfig {
    AgentConfig {
        model: None,
        max_tool_calls: 8,
        task_deadline_seconds: 5,
        daily_request_cap: 100,
    }
}

async fn opened(server: &mut Server, thread: &str, expected: &Catalog) {
    let request = read(server).await;
    assert_eq!(request["method"], "thread/start");
    assert_eq!(
        request["params"]["dynamicTools"],
        serde_json::to_value(expected.tools()).unwrap()
    );
    write(server, json!({"id":request["id"],"result":{"thread":{"id":thread}}})).await;
    let request = read(server).await;
    assert_eq!(request["method"], "thread/realtime/start");
    assert_eq!(request["params"]["threadId"], thread);
    notification(
        server,
        "thread/realtime/sdp",
        json!({"threadId":thread,"sdp":"fixture-answer"}),
    )
    .await;
    write(server, json!({"id":request["id"],"result":{}})).await;
    notification(server, "turn/started", json!({"threadId":thread,"turn":{"id":"turn"}})).await;
}

async fn tool(server: &mut Server, thread: &str, id: &str, name: &str, args: Value) -> Value {
    write(
        server,
        json!({"id":id,"method":"item/tool/call","params":{
            "threadId":thread,"turnId":"turn","callId":id,"tool":name,"arguments":args
        }}),
    )
    .await;
    let response = read(server).await;
    assert_eq!(response["id"], id);
    response["result"].clone()
}

fn result(content: &Value) -> Value {
    json!({"success":true,"contentItems":[{"type":"inputText","text":content.to_string()}]})
}

async fn settled<R, W, P, H>(session: &mut ToolSession<'_, '_, R, W, P, H>)
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
    P: MediaPeer,
    H: ToolHost + 'static,
{
    loop {
        match session.next().await.unwrap() {
            MediaEvent::Control(Event::ReplyDone { text }) => {
                assert_eq!(text, "done");
                return;
            }
            MediaEvent::Control(Event::Other | Event::TurnStarted { .. } | Event::TurnCompleted { .. }) => (),
            _ => unreachable!("unexpected session event"),
        }
    }
}

async fn closed(server: &mut Server, thread: &str) {
    notification(
        server,
        "turn/completed",
        json!({"threadId":thread,"turn":{"id":"turn"}}),
    )
    .await;
    notification(
        server,
        "thread/realtime/transcript/done",
        json!({"threadId":thread,"role":"assistant","text":"done"}),
    )
    .await;
    let request = read(server).await;
    assert_eq!(request["method"], "thread/realtime/stop");
    notification(server, "thread/realtime/closed", json!({"threadId":thread})).await;
    write(server, json!({"id":request["id"],"result":{}})).await;
}

async fn two_calls(io: &mut Io, home: &HomeConfig, trace: &Trace) {
    for _ in 0..2 {
        let (peer, _, _tx) = peer(true, None);
        let notes = Notes::new(Some(home), trace.clone());
        let mut call = MediaCall::new(io, peer);
        call.trace_tools(trace.clone(), &catalog(&notes));
        call.start(&options(), catalog(&notes).tools().to_vec(), deadline())
            .await
            .unwrap();
        let host = Local {
            host: NoMcp,
            notes: notes.clone(),
        };
        let mut session = ToolSession::new(
            &mut call,
            host,
            catalog(&notes),
            config(),
            ToolBudget::new(100, || Ok(0)),
        )
        .unwrap();
        settled(&mut session).await;
        drop(session);
        call.close(deadline()).await.unwrap();
    }
}

async fn scripted(home: Option<&HomeConfig>, trace: &Trace, steps: Vec<(&str, Value, Value)>) {
    let notes = Notes::new(home, trace.clone());
    let (mut io, mut server) = fixture();
    let client = drive(&mut io, &notes, trace);
    let remote = async {
        opened(&mut server, "thread", &catalog(&notes)).await;
        for (index, (name, args, expected)) in steps.into_iter().enumerate() {
            assert_eq!(
                tool(&mut server, "thread", &format!("call-{index}"), name, args).await,
                expected
            );
        }
        closed(&mut server, "thread").await;
    };
    tokio::join!(client, remote);
}

async fn drive(io: &mut Io, notes: &Notes, trace: &Trace) {
    let (peer, _, _audio) = peer(true, None);
    let mut call = MediaCall::new(io, peer);
    call.trace_tools(trace.clone(), &catalog(notes));
    call.start(&options(), catalog(notes).tools().to_vec(), deadline())
        .await
        .unwrap();
    let mut session = ToolSession::new(
        &mut call,
        Local {
            host: NoMcp,
            notes: notes.clone(),
        },
        catalog(notes),
        config(),
        ToolBudget::new(100, || Ok(0)),
    )
    .unwrap();
    settled(&mut session).await;
    drop(session);
    call.close(deadline()).await.unwrap();
}

async fn writing(server: &mut Server, notes: &Notes, home: &HomeConfig) {
    opened(server, "thread-a", &catalog(notes)).await;
    assert_eq!(
        tool(
            server,
            "thread-a",
            "write",
            "notes_write",
            json!({"path":"house/preferences.md","content":"remember"})
        )
        .await,
        result(&json!({"path":"house/preferences.md","bytes":8}))
    );
    assert_eq!(
        tool(
            server,
            "thread-a",
            "write",
            "notes_write",
            json!({"path":"house/preferences.md","content":"replayed"})
        )
        .await,
        json!({"success":false,"contentItems":[{"type":"inputText","text":"stale or duplicate tool request"}]})
    );
    closed(server, "thread-a").await;
    std::fs::File::open(home.dir.join("notes/house/preferences.md"))
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1))
        .unwrap();
}

async fn reading(server: &mut Server, notes: &Notes) {
    opened(server, "thread-b", &catalog(notes)).await;
    assert_eq!(
        tool(
            server,
            "thread-b",
            "read",
            "notes_read",
            json!({"path":"house/preferences.md"})
        )
        .await,
        result(&json!({"path":"house/preferences.md","bytes":8,"content":"remember"}))
    );
    assert_eq!(
        tool(server, "thread-b", "list", "notes_list", json!({})).await,
        result(&json!({"files":[{"path":"house/preferences.md","bytes":8,"modified_unix_ms":1000}]}))
    );
    closed(server, "thread-b").await;
}

#[tokio::test(start_paused = true)]
async fn t3_agent_tools_notes_cross_thread_exact_bytes_metadata_and_replay_refusal() {
    let root = tempfile::tempdir().unwrap();
    let trace = Trace::open(&root.path().join("trace"), "api_key").unwrap();
    trace.tool_run(Some("notes-journey")).unwrap();
    let home = HomeConfig {
        dir: root.path().join("home"),
        agent_tools: AgentTools::default(),
    };
    let notes = Notes::new(Some(&home), trace.clone());
    let (mut io, mut server) = fixture();
    let client = two_calls(&mut io, &home, &trace);
    let remote = async {
        writing(&mut server, &notes, &home).await;
        reading(&mut server, &notes).await;
    };
    tokio::join!(client, remote);
    let rows: Vec<Value> = std::fs::read_to_string(root.path().join("trace"))
        .unwrap()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .filter(|row: &Value| row["event"] == "agent.tool")
        .collect();
    assert_eq!(
        rows.iter().map(|row| row["tool"].as_str().unwrap()).collect::<Vec<_>>(),
        ["notes_write", "notes_read", "notes_list"]
    );
    assert!(
        rows.iter()
            .all(|row| row["run_id"] == "notes-journey" && row["result"] == "ok" && row["bytes"] == 8)
    );
}

#[tokio::test(start_paused = true)]
async fn t3_agent_tools_notes_disabled_forged_request_has_zero_effects() {
    let root = tempfile::tempdir().unwrap();
    let trace = Trace::open(&root.path().join("trace"), "api_key").unwrap();
    let home = HomeConfig {
        dir: root.path().join("home"),
        agent_tools: AgentTools {
            notes: false,
            prompts: false,
        },
    };
    let notes = Notes::new(Some(&home), trace);
    let (mut io, mut server) = fixture();
    let (peer, _, _tx) = peer(true, None);
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        call.start(&options(), catalog(&notes).tools().to_vec(), deadline())
            .await
            .unwrap();
        let mut session = ToolSession::new(
            &mut call,
            Local {
                host: NoMcp,
                notes: notes.clone(),
            },
            catalog(&notes),
            config(),
            ToolBudget::new(100, || Ok(0)),
        )
        .unwrap();
        settled(&mut session).await;
        drop(session);
        call.close(deadline()).await.unwrap();
    };
    let remote = async {
        opened(&mut server, "thread", &catalog(&notes)).await;
        for name in NAMES {
            let response = tool(
                &mut server,
                "thread",
                name,
                name,
                json!({"path":"x.md","content":"forged"}),
            )
            .await;
            assert_eq!(
                response,
                json!({"success":false,"contentItems":[{"type":"inputText","text":"tool request refused"}]})
            );
        }
        closed(&mut server, "thread").await;
    };
    tokio::join!(client, remote);
    assert!(!home.dir.exists());
}
