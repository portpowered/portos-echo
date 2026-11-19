//! T3 production admission, catalog, dispatcher, shared writer and next-call RPC.
use super::*;
use crate::{bridge_runtime::bridge_screen::Screen, bridge_voice::voice_tools::Tools};
use echo_config::{AgentTools, BridgeConfig, HomeConfig};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

fn config(home: &Path, selection: AgentTools) -> BridgeConfig {
    let mut config =
        BridgeConfig::from_toml_str(include_str!("../../../../../../examples/bridge.example.toml")).unwrap();
    config.mcp.server.clear();
    config.home = Some(HomeConfig {
        dir: home.into(),
        agent_tools: selection,
    });
    config.audit.log_dir = home.parent().unwrap().join("audit");
    config
}

#[tokio::test(start_paused = true)]
async fn t3_agent_tools_prompt_writes_preserve_current_and_change_exact_next_rpc() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    prompts::write_base(&home, "voice", "voice old").unwrap();
    prompts::write_base(&home, "runner", "runner old").unwrap();
    let path = root.path().join("trace");
    let mut trace = Trace::open(&path, "api_key").unwrap();
    trace.tool_run(Some("prompt-journey")).unwrap();
    let config = config(&home, AgentTools::default());
    let screen = Screen::bind(None).await.unwrap();
    let mut tools = Tools::new(&config, Arc::new(|| 0));
    tools.open(&config, &screen, &mut trace, None).await.unwrap();
    prompts::scope(Some(&home), async {
        exercise(&home, &trace, &mut tools).await;
        Ok(())
    })
    .await
    .unwrap();
    tools.close().await;
    let history = home.join("prompts/.history");
    let mut old: Vec<_> = std::fs::read_dir(history)
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect();
    old.sort();
    assert_eq!(old, ["runner old", "voice old"]);
    privacy(&path);
}

async fn exercise(home: &Path, trace: &Trace, tools: &mut Tools) {
    let (stream, server) = tokio::io::duplex(262_144);
    let (read, write) = tokio::io::split(stream);
    let mut io = RpcIo::new(read, write, echo_codex::codex_rpc::Limits::default()).unwrap();
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    let mut options = admission_options();
    options.call.time_zone = None;
    let mut server = BufReader::new(server);
    for index in 0..3 {
        observer.wake("admit", vec![]);
        let LinkEvent::Wake(run) = events.recv().await.unwrap().unwrap() else {
            unreachable!()
        };
        let started = tokio::sync::Notify::new();
        let mut completed = 0;
        let mut observe = retained_observer(&observer, &started, &mut completed);
        let mut peer = async || Ok(MemoryPeer);
        let expected_catalog = serde_json::to_value(tools.catalog().await.unwrap().tools()).unwrap();
        let work = Box::pin(admitted(
            (&client, &mut events, run),
            &mut io,
            &mut options,
            &mut peer,
            std::future::pending(),
            &mut observe,
            (Some(&mut *tools), || std::time::UNIX_EPOCH, Some(trace.clone())),
        ));
        let remote = remote(&mut server, (&observer, &started), index, &expected_catalog, home);
        let (result, ()) = tokio::join!(work, remote);
        assert_eq!(result, Ok(()));
        drop(observe);
        assert_eq!(completed, 2);
    }
}

const VOICE: &str = "AGENT_PROMPT_PRIVATE_MARKER voice new\n語";
const RUNNER: &str = "runner new\n語";
const NOTE: &str = "AGENT_NOTE_PRIVATE_MARKER";

async fn remote(
    server: &mut BufReader<tokio::io::DuplexStream>,
    run: (&Observer, &tokio::sync::Notify),
    index: usize,
    catalog: &Value,
    home: &Path,
) {
    let expected = if index == 0 {
        ("runner old", "voice old")
    } else {
        (RUNNER, VOICE)
    };
    let thread = format!("thread-{index}");
    for method in ["thread/start", "thread/realtime/start", "thread/realtime/stop"] {
        let mut line = String::new();
        server.read_line(&mut line).await.unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], method);
        if method == "thread/start" {
            assert_eq!(request["params"]["dynamicTools"], *catalog);
        }
        let result = prompt_result(server, &request, method, &thread, expected).await;
        send(server, &json!({"id":request["id"],"result":result})).await;
        if method == "thread/realtime/start" {
            send(
                server,
                &json!({"method":"turn/started","params":{"threadId":thread,"turn":{"id":"turn"}}}),
            )
            .await;
            if index == 0 {
                changes(server, &thread).await;
            } else if index == 1 {
                refusals(server, &thread).await;
            } else {
                notes(server, &thread, home).await;
            }
            send(
                server,
                &json!({"method":"turn/completed","params":{"threadId":thread,"turn":{"id":"turn"}}}),
            )
            .await;
            // Two replies on this retained call must produce no new start or
            // instruction RPC. The next loop iteration checks both exact texts.
            replies(server, run.0, run.1, &thread).await;
        }
    }
}

async fn tool(
    server: &mut BufReader<tokio::io::DuplexStream>,
    thread: &str,
    name: &str,
    args: Value,
    id: &str,
) -> Value {
    send(
        server,
        &json!({"id":id,"method":"item/tool/call","params":{
            "threadId":thread,"turnId":"turn","callId":id,"tool":name,"arguments":args
        }}),
    )
    .await;
    let mut line = String::new();
    server.read_line(&mut line).await.unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["id"], id);
    response["result"].clone()
}

fn answer(content: &Value, success: bool) -> Value {
    json!({"success":success,"contentItems":[{"type":"inputText","text":content.to_string()}]})
}

async fn changes(server: &mut BufReader<tokio::io::DuplexStream>, thread: &str) {
    for (which, text) in [("voice", VOICE), ("runner", RUNNER)] {
        assert_eq!(
            tool(
                server,
                thread,
                "prompt_write",
                json!({"which":which,"content":text}),
                which
            )
            .await,
            answer(
                &json!({"which":which,"bytes":text.len(),"applies":"next_conversation"}),
                true
            )
        );
        assert_eq!(
            tool(
                server,
                thread,
                "prompt_read",
                json!({"which":which}),
                &format!("read-{which}")
            )
            .await,
            answer(&json!({"which":which,"bytes":text.len(),"content":text}), true)
        );
    }
    assert_eq!(
        tool(
            server,
            thread,
            "notes_write",
            json!({"path":"house/preferences.md","content":NOTE}),
            "note"
        )
        .await,
        answer(&json!({"path":"house/preferences.md","bytes":NOTE.len()}), true)
    );
    // A replay cannot create a second archive or another mutation.
    assert_eq!(
        tool(
            server,
            thread,
            "prompt_write",
            json!({"which":"voice","content":"replayed"}),
            "voice"
        )
        .await["success"],
        false
    );
}

async fn refusals(server: &mut BufReader<tokio::io::DuplexStream>, thread: &str) {
    for (index, text) in ["".into(), " \n\t".into(), "x".repeat(32_769)].into_iter().enumerate() {
        assert_eq!(
            tool(
                server,
                thread,
                "prompt_write",
                json!({"which":"voice","content":text}),
                &format!("bad-{index}")
            )
            .await,
            answer(
                &json!({"error":"invalid_prompt","message":"A prompt must be nonblank and at most 32768 bytes."}),
                false
            )
        );
    }
    assert_eq!(
        tool(server, thread, "prompt_read", json!({"which":"voice"}), "read").await,
        answer(&json!({"which":"voice","bytes":VOICE.len(),"content":VOICE}), true)
    );
}

async fn notes(server: &mut BufReader<tokio::io::DuplexStream>, thread: &str, home: &Path) {
    assert_eq!(
        tool(
            server,
            thread,
            "notes_read",
            json!({"path":"house/preferences.md"}),
            "read"
        )
        .await,
        answer(
            &json!({"path":"house/preferences.md","bytes":NOTE.len(),"content":NOTE}),
            true
        )
    );
    let modified = std::fs::metadata(home.join("notes/house/preferences.md"))
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    assert_eq!(
        tool(server, thread, "notes_list", json!({}), "list").await,
        answer(
            &json!({"files":[{"path":"house/preferences.md","bytes":NOTE.len(),"modified_unix_ms":modified}]}),
            true
        )
    );
}

fn privacy(path: &Path) {
    let text = std::fs::read_to_string(path).unwrap();
    assert_eq!(text.matches(NOTE).count(), 0);
    assert_eq!(text.matches("AGENT_PROMPT_PRIVATE_MARKER").count(), 0);
    let rows: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| row["event"] == "agent.tool")
        .collect();
    assert_eq!(rows.len(), 11);
    let prompt_rows: Vec<_> = rows
        .iter()
        .filter(|row| row["tool"].as_str().unwrap().starts_with("prompt_"))
        .collect();
    assert_eq!(prompt_rows.len(), 8);
    for row in prompt_rows {
        let valid = row["result"] == "ok";
        let which = if valid { row["which"].as_str().unwrap() } else { "voice" };
        let bytes = if valid {
            if which == "voice" { VOICE.len() } else { RUNNER.len() }
        } else {
            0
        };
        assert_eq!(
            *row,
            json!({"event":"agent.tool","elapsed_ms":row["elapsed_ms"],"provider_auth_mode":"api_key",
            "run_id":"prompt-journey","tool":row["tool"],"which":if valid {json!(which)} else {Value::Null},
            "bytes":bytes,"result":if valid {"ok"} else {"invalid_prompt"}})
        );
    }
}

#[tokio::test(start_paused = true)]
async fn t3_agent_tools_independent_groups_catalog_and_disabled_calls_are_inert() {
    let root = tempfile::tempdir().unwrap();
    for selection in [
        AgentTools {
            notes: false,
            prompts: false,
        },
        AgentTools {
            notes: true,
            prompts: false,
        },
        AgentTools {
            notes: false,
            prompts: true,
        },
    ] {
        let home = root.path().join(format!("{}-{}", selection.notes, selection.prompts));
        let config = config(&home, selection);
        let screen = Screen::bind(None).await.unwrap();
        let mut trace = Trace::open(&root.path().join("trace"), "api_key").unwrap();
        let mut tools = Tools::new(&config, Arc::new(|| 0));
        tools.open(&config, &screen, &mut trace, None).await.unwrap();
        let catalog = tools.catalog().await.unwrap();
        let names: Vec<_> = catalog.tools().iter().map(|tool| tool.name.as_str()).collect();
        let mut expected = vec!["extend_session", "end_session"];
        if selection.notes {
            expected.extend(["notes_list", "notes_read", "notes_write"]);
        }
        if selection.prompts {
            expected.extend(["prompt_read", "prompt_write"]);
        }
        assert_eq!(names, expected);
        use echo_agent_api::ToolHost;
        for name in crate::bridge_notes::NAMES {
            if !names.contains(&name) {
                let result = tools
                    .host
                    .as_ref()
                    .unwrap()
                    .call_tool(name, json!({"which":"voice","path":"x.md","content":"x"}), "1:t1:u1:c")
                    .await
                    .unwrap();
                assert_eq!(
                    result.content,
                    json!({"error":"disabled","message":"This tool group is disabled."})
                );
                assert!(result.is_error);
            }
        }
        assert!(!home.exists());
        tools.close().await;
    }
}
