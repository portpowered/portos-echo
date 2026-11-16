//! T3: actual configured prebuilt bridge + Live/Responses/MCP loopback peers.
//! A controlled Wyoming peer is used here, not a pinned-host milestone claim.
use super::model::{FALLBACK, Model, SUCCESS};
use crate::lab_run::sim::process::Process;
use echo_contracts::wyoming::{Decoder, Event, Header, encode};
use echo_live::fake::{DialogueScript, DialogueSent, FakeLive};
use echo_spoof_mcp::{Fault, Options, Spoof};
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    process::Command,
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "lab-m4-wire-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn configure(root: &Root, port: u16, spoof: &Spoof) {
    let config = format!(
        r#"
[echo]
host = "127.0.0.1"
wyoming_port = {port}
[live]
mode = "api_key"
credential_file = "credential"
voice = "alloy"
session_idle_seconds = 120
[agent]
model = "fixture-model"
max_tool_calls = 8
task_deadline_seconds = 10
daily_request_cap = 10
[[mcp.server]]
name = "synthetic"
url = "{}"
auth = "none"
allow_tools = ["test.echo"]
[screen]
listen = "127.0.0.1:0"
pairing_token_file = "pairing"
[audit]
log_dir = "audit"
[chatgpt_auth]
token_endpoint = "http://127.0.0.1:1/token"
"#,
        spoof.url()
    );
    std::fs::write(root.0.join("bridge.toml"), config).unwrap();
    std::fs::write(root.0.join("credential"), "synthetic-m4-key").unwrap();
    std::fs::write(root.0.join("pairing"), "test-pair").unwrap();
}

fn command(root: &Root, live: &str, model: &str) -> Command {
    let bridge = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug")
        .join(if cfg!(windows) {
            "echo-bridge.exe"
        } else {
            "echo-bridge"
        });
    assert!(bridge.is_file(), "build the real echo-bridge before T3 tests");
    let mut command = Command::new(bridge);
    command.current_dir(&root.0).args([
        "--config",
        "bridge.toml",
        "--trace",
        "trace",
        "--health-listen",
        "127.0.0.1:0",
        "--live-loopback",
        live,
        "--model-loopback",
        model,
    ]);
    for key in [
        "ECHO_LAB_WYOMING_ADDR",
        "ECHO_LAB_LIVE_URL",
        "ECHO_LAB_TRACE_PATH",
        "ECHO_LAB_SESSION_IDLE_SECONDS",
    ] {
        command.env_remove(key);
    }
    command
}

async fn send(peer: &mut TcpStream, name: &str, data: Value) {
    let event = Event {
        header: Header {
            r#type: name.into(),
            data: Some(data),
            data_length: 0,
            payload_length: 0,
        },
        data: None,
        payload: vec![],
    };
    peer.write_all(&encode(&event)).await.unwrap();
}

async fn receive(peer: &mut TcpStream, decoder: &mut Decoder) -> Event {
    loop {
        let byte = peer.read_u8().await.unwrap();
        if let Some(value) = decoder.feed(&[byte]).unwrap().pop() {
            return value;
        }
    }
}
async fn journey(negative: bool) {
    let root = Root::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let spoof = Spoof::start(Options {
        fault: if negative { Fault::Http500 } else { Fault::None },
        ..Options::default()
    })
    .await
    .unwrap();
    configure(&root, listener.local_addr().unwrap().port(), &spoof);
    let model = Model::bind(negative).await.unwrap();
    let text = if negative { FALLBACK } else { SUCCESS };
    let live = FakeLive::bind_dialogue(DialogueScript {
        delegation_id: "d-1".into(),
        input: "echo".into(),
        commentary: text.into(),
        transcript: text.into(),
        audio_frames: 10,
    })
    .await
    .unwrap();
    let observer = live.observer();
    let mut bridge = Process::start(&mut command(&root, live.base_url(), &model.origin)).unwrap();
    exercise(&root, &listener, &mut bridge, &observer, text).await;
    assert_journey(&root, &model, &spoof);
    live.shutdown().await.unwrap();
    model.close().await.unwrap();
    spoof.shutdown().await.unwrap();
}

fn assert_journey(root: &Root, model: &Model, spoof: &Spoof) {
    assert_eq!(model.check(), Ok(()));
    let snapshot = spoof.snapshot().unwrap();
    assert_eq!(snapshot.calls.len(), 1);
    assert_eq!(snapshot.calls[0].name, "test.echo");
    assert_eq!(snapshot.calls[0].args, json!({"text":"delegated echo"}));
    assert_eq!(snapshot.calls[0].count, 1);
    let raw = std::fs::read_to_string(root.0.join("trace")).unwrap();
    let trace: Vec<Value> = raw.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_recorded_comparison(&raw);
    let edges: Vec<_> = trace
        .iter()
        .filter(|event| {
            matches!(
                event["event"].as_str(),
                Some("session.delegation.created" | "tools/call" | "session.commentary.append")
            )
        })
        .collect();
    assert_eq!(
        edges.iter().map(|e| e["event"].as_str().unwrap()).collect::<Vec<_>>(),
        ["session.delegation.created", "tools/call", "session.commentary.append"]
    );
    for edge in edges {
        assert_eq!(
            (&edge["run_id"], &edge["delegation_id"]),
            (&json!("run-1"), &json!("d-1"))
        );
    }
    for forbidden in ["synthetic-m4-key", "test-pair", "delegated echo"] {
        assert!(!raw.contains(forbidden));
    }
}

fn assert_recorded_comparison(raw: &str) {
    let mut actual = crate::lab_run::sim::trace::Trace::default();
    actual.append(raw.as_bytes()).unwrap();
    let recorded = format!("{}\n", super::golden::normalize(&actual).unwrap().join("\n"));
    assert_eq!(super::golden::compare(&actual, recorded.as_bytes()), Ok(()));
    let index = actual.records.iter().position(|r| r["event"] == "tools/call").unwrap();
    actual.records[index]["name"] = json!("test.wait");
    let error = super::golden::compare(&actual, recorded.as_bytes()).unwrap_err();
    assert!(error.contains(&format!("line {}\n- ", index + 1)), "{error}");
    assert!(error.contains("\"name\":\"test.echo\""), "{error}");
    assert!(error.contains("\"name\":\"test.wait\""), "{error}");
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn t3_m4_configured_bridge_exact_tool_result_releases_positive_dialogue() {
    tokio::time::timeout(std::time::Duration::from_secs(20), journey(false))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn t3_m4_fresh_http500_result_releases_honest_fallback_without_retry() {
    tokio::time::timeout(std::time::Duration::from_secs(20), journey(true))
        .await
        .unwrap();
}

fn assert_dialogue(observer: &echo_live::fake::DialogueObserver, text: &str) {
    let calls = observer.calls().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].delegation_id, "d-1");
    assert_eq!(calls[0].rejected_commentary, 0);
    assert_eq!(calls[0].failure, None);
    assert_eq!(
        calls[0]
            .sent
            .iter()
            .filter(|sent| matches!(sent, DialogueSent::Audio(_)))
            .count(),
        10
    );
    let transcript = calls[0]
        .sent
        .iter()
        .find_map(|sent| match sent {
            DialogueSent::Text(text) => serde_json::from_str::<Value>(text)
                .ok()
                .filter(|v| v["type"] == "response.audio_transcript.done"),
            DialogueSent::Audio(_) => None,
        })
        .unwrap();
    assert_eq!(transcript["transcript"], text);
}

async fn register(listener: &TcpListener, bridge: &mut Process) -> (TcpStream, Decoder) {
    let mut peer = tokio::time::timeout(std::time::Duration::from_secs(3), listener.accept())
        .await
        .expect("bridge Wyoming connect")
        .unwrap()
        .0;
    let mut decoder = Decoder::new();
    assert_eq!(receive(&mut peer, &mut decoder).await.header.r#type, "run-satellite");
    assert_eq!(receive(&mut peer, &mut decoder).await.header.r#type, "describe");
    send(&mut peer, "info", json!({"satellite":{"name":"synthetic"}})).await;
    bridge.expect("bridge.ready").await.unwrap();
    (peer, decoder)
}

async fn receive_reply(peer: &mut TcpStream, decoder: &mut Decoder) {
    assert_eq!(receive(peer, decoder).await.header.r#type, "audio-start");
    let mut bytes = Vec::new();
    let mut chunks = 0;
    loop {
        let event = receive(peer, decoder).await;
        if event.header.r#type == "audio-stop" {
            break;
        }
        assert_eq!(event.header.r#type, "audio-chunk");
        assert_eq!(event.header.data, Some(json!({"rate":24000,"width":2,"channels":1})));
        chunks += 1;
        bytes.extend(event.payload);
    }
    assert_eq!(chunks, 10);
    assert_eq!(bytes, vec![0; 9600]);
}

type Screen = tokio::io::Lines<tokio::io::BufReader<TcpStream>>;
async fn screen(root: &Root) -> Screen {
    use tokio::io::AsyncBufReadExt;
    let raw = std::fs::read_to_string(root.0.join("trace")).unwrap();
    let address = raw
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|e| e["event"] == "screen.listening")
        .unwrap()["address"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut socket = TcpStream::connect(address).await.unwrap();
    socket
        .write_all(b"GET /events?token=test-pair HTTP/1.1\r\nHost: local\r\n\r\n")
        .await
        .unwrap();
    let mut lines = tokio::io::BufReader::new(socket).lines();
    assert_eq!(lines.next_line().await.unwrap().unwrap(), "HTTP/1.1 200 OK");
    lines
}
async fn state(screen: &mut Screen) -> Value {
    loop {
        let line = screen
            .next_line()
            .await
            .unwrap()
            .expect("screen EOF before observation");
        if let Some(value) = line.strip_prefix("data: ") {
            return serde_json::from_str::<Value>(value).unwrap()["state"].clone();
        }
    }
}

async fn exercise(
    root: &Root,
    listener: &TcpListener,
    bridge: &mut Process,
    observer: &echo_live::fake::DialogueObserver,
    text: &str,
) {
    let (mut peer, mut decoder) = register(listener, bridge).await;
    let mut screen = screen(root).await;
    assert_eq!(state(&mut screen).await, json!({"state":"ready","message":"Ready"}));
    send(
        &mut peer,
        "run-pipeline",
        json!({"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"computer"}),
    )
    .await;
    finish_reply(&mut peer, &mut decoder, &mut screen, bridge, observer, text).await;
    bridge.close().await.unwrap();
}

async fn finish_reply(
    peer: &mut TcpStream,
    decoder: &mut Decoder,
    screen: &mut Screen,
    bridge: &mut Process,
    observer: &echo_live::fake::DialogueObserver,
    text: &str,
) {
    observer.wait_for(0, |call| call.reply_completed).await.unwrap();
    assert_dialogue(observer, text);
    receive_reply(peer, decoder).await;
    assert_eq!(state(screen).await, json!({"state":"listening"}));
    assert_eq!(state(screen).await, json!({"state":"speaking"}));
    send(peer, "played", json!({})).await;
    send(peer, "audio-stop", json!({})).await;
    assert_eq!(bridge.expect("bridge.run_completed").await.unwrap()["run_id"], "run-1");
    assert_eq!(state(screen).await, json!({"state":"ready","message":"Ready"}));
}
