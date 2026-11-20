//! T1: signaling component with in-memory JSONL IO and a paused clock.
use super::*;
use serde_json::Value;
use std::io::Write;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

pub(super) type Io = RpcIo<ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;
pub(super) type Server = BufReader<DuplexStream>;

pub(super) fn fixture() -> (Io, Server) {
    let (client, server) = tokio::io::duplex(65_536);
    let (reader, writer) = tokio::io::split(client);
    (
        RpcIo::new(reader, writer, echo_codex::codex_rpc::Limits::default()).unwrap(),
        BufReader::new(server),
    )
}

pub(super) fn options() -> Options<'static> {
    Options {
        voice: Some("alloy"),
        instructions: "Client audio bridge",
        prompt: "Live prompt",
        model: Some("fixture-model"),
        effort: None,
        time_zone: None,
        web_search: echo_config::WebSearch::default(),
    }
}

pub(super) fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

pub(super) async fn read(server: &mut Server) -> Value {
    let mut line = String::new();
    assert_ne!(server.read_line(&mut line).await.unwrap(), 0);
    serde_json::from_str(&line).unwrap()
}

pub(super) async fn write(server: &mut Server, value: Value) {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    server.get_mut().write_all(&bytes).await.unwrap();
}

pub(super) async fn notification(server: &mut Server, method: &str, params: Value) {
    write(server, json!({"method":method,"params":params})).await;
}

pub(super) async fn thread(server: &mut Server) -> Value {
    let request = read(server).await;
    assert_eq!(request["method"], "thread/start");
    writeln!(std::io::stdout(), "observed thread/start: {request}").unwrap();
    assert_eq!(request.get("jsonrpc"), None);
    write(
        server,
        json!({"id":request["id"],"result":{"thread":{"id":"thread-1"}}}),
    )
    .await;
    request["params"].clone()
}

pub(super) async fn start(server: &mut Server) -> Value {
    let params = thread(server).await;
    let request = read(server).await;
    assert_eq!(request["method"], "thread/realtime/start");
    assert_eq!(
        request["params"],
        json!({
            "threadId":"thread-1","outputModality":"audio","transport":{"type":"webrtc","sdp":"fixture-offer"},
            "version":"v3","includeStartupContext":false,"clientManagedHandoffs":false,
            "voice":"alloy","prompt":"Live prompt"
        })
    );
    // Real app servers can emit SDP before the request's reply.
    notification(
        server,
        "thread/realtime/sdp",
        json!({"threadId":"foreign","sdp":"wrong"}),
    )
    .await;
    notification(
        server,
        "thread/realtime/sdp",
        json!({"threadId":"thread-1","sdp":"fixture-answer"}),
    )
    .await;
    write(server, json!({"id":request["id"],"result":{}})).await;
    params
}

// Service admission adds a fresh snapshot; direct signaling keeps `start` above.
pub(super) async fn admitted_start(server: &mut Server, began: std::time::SystemTime) {
    let params = thread(server).await;
    let request = read(server).await;
    let ended = std::time::SystemTime::now();
    let prompt = params["baseInstructions"].as_str().unwrap();
    assert_snapshot(prompt, began, ended);
    let mut expected = serde_json::to_value(ThreadSettings::new(vec![])).unwrap();
    expected["model"] = "fixture-model".into();
    expected["baseInstructions"] = prompt.into();
    assert_eq!(params, expected);
    assert_eq!(request["method"], "thread/realtime/start");
    assert_eq!(
        request["params"],
        json!({
            "threadId":"thread-1","outputModality":"audio","transport":{"type":"webrtc","sdp":"fixture-offer"},
            "version":"v3","includeStartupContext":false,"clientManagedHandoffs":false,
            "voice":"alloy","prompt":prompt.replacen("Client audio bridge", "Live prompt", 1)
        })
    );
    notification(
        server,
        "thread/realtime/sdp",
        json!({"threadId":"foreign","sdp":"wrong"}),
    )
    .await;
    notification(
        server,
        "thread/realtime/sdp",
        json!({"threadId":"thread-1","sdp":"fixture-answer"}),
    )
    .await;
    write(server, json!({"id":request["id"],"result":{}})).await;
}

fn assert_snapshot(prompt: &str, began: std::time::SystemTime, ended: std::time::SystemTime) {
    let snapshot = prompt
        .strip_prefix("Client audio bridge\n[Call-start context: ")
        .unwrap();
    let snapshot = snapshot.strip_suffix(" UTC+00:00; local offset unavailable]").unwrap();
    let fields: Vec<_> = snapshot.split(' ').collect();
    assert_eq!(fields.len(), 3);
    let date: Vec<u64> = fields[0].split('-').map(|part| part.parse().unwrap()).collect();
    let time: Vec<u64> = fields[2].split(':').map(|part| part.parse().unwrap()).collect();
    assert_eq!(date.len(), 3);
    assert_eq!(time.len(), 3);
    let (year, month, day) = (date[0], date[1], date[2]);
    assert!((1970..=9999).contains(&year));
    assert!((1..=12).contains(&month));
    let leap = |y| y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let months = [
        31,
        if leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    assert!((1..=months[usize::try_from(month - 1).unwrap()]).contains(&day));
    assert!(time[0] < 24 && time[1] < 60 && time[2] < 60);
    let days = (1970..year).map(|y| if leap(y) { 366 } else { 365 }).sum::<u64>()
        + months[..usize::try_from(month - 1).unwrap()].iter().sum::<u64>()
        + day
        - 1;
    let weekdays = [
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
    ];
    let weekday = weekdays[usize::try_from(days % 7).unwrap()];
    assert_eq!(
        snapshot,
        format!(
            "{year:04}-{month:02}-{day:02} {weekday} {:02}:{:02}:{:02}",
            time[0], time[1], time[2]
        )
    );
    let epoch = days * 86_400 + time[0] * 3600 + time[1] * 60 + time[2];
    // The wire snapshot has second precision; compare equally truncated bounds.
    assert!(epoch >= began.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs());
    assert!(epoch <= ended.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs());
}

pub(super) async fn stop(server: &mut Server) {
    let request = read(server).await;
    assert_eq!(request["method"], "thread/realtime/stop");
    assert_eq!(request["params"], json!({"threadId":"thread-1"}));
    notification(server, "thread/realtime/closed", json!({"threadId":"foreign"})).await;
    // The matching close may precede the acknowledgement, too.
    notification(server, "thread/realtime/closed", json!({"threadId":"thread-1"})).await;
    write(server, json!({"id":request["id"],"result":{}})).await;
}

#[tokio::test(start_paused = true)]
async fn call_start_preserves_trimmed_thread_v3_options_and_exact_tool_schema() {
    let (mut io, mut server) = fixture();
    let tool = DynamicTool {
        name: "tool_0".into(),
        description: "Echo text".into(),
        input_schema: json!({"type":"object"}),
    };
    let expected = tool.clone();
    let client = async {
        let mut call = Call::new(&mut io);
        assert_eq!(
            call.start(&options(), vec![tool], "fixture-offer", deadline())
                .await
                .unwrap(),
            "fixture-answer"
        );
        assert_eq!(call.stop(deadline()).await, Ok(()));
        assert_eq!(call.stop(deadline()).await, Ok(()));
        assert!(matches!(call.next().await, Err(Error::Unavailable)));
        assert_eq!(
            call.start(&options(), vec![], "fixture-offer", deadline()).await,
            Err(Error::Input)
        );
    };
    let remote = async {
        let params = start(&mut server).await;
        let mut exact = serde_json::to_value(ThreadSettings::new(vec![expected])).unwrap();
        exact["model"] = "fixture-model".into();
        exact["baseInstructions"] = "Client audio bridge".into();
        assert_eq!(params, exact);
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
    // Graceful stop retains a usable supervisor generation, without replay.
    assert!(io.begin_request("account/read", &json!({})).is_ok());
}

#[tokio::test(start_paused = true)]
async fn call_transcript_end_requires_matching_thread_and_assistant_role() {
    let (mut io, mut server) = fixture();
    let client = async {
        let mut call = Call::new(&mut io);
        call.start(&options(), vec![], "fixture-offer", deadline())
            .await
            .unwrap();
        assert!(matches!(call.next().await.unwrap(), Event::Other));
        assert!(matches!(call.next().await.unwrap(), Event::Other));
        assert!(
            matches!(call.next().await.unwrap(), Event::Transcript { role, text, done:true }
            if role == "user" && text == "hello")
        );
        assert!(
            matches!(call.next().await.unwrap(), Event::Transcript { role, text, done:false }
            if role == "assistant" && text == "re")
        );
        assert!(matches!(call.next().await.unwrap(), Event::ReplyDone { text } if text == "reply"));
        call.stop(deadline()).await.unwrap();
    };
    let remote = async {
        start(&mut server).await;
        notification(
            &mut server,
            "thread/realtime/transcript/done",
            json!({"threadId":"foreign","role":"assistant","text":"wrong"}),
        )
        .await;
        notification(&mut server, "response.done", json!({"threadId":"thread-1"})).await;
        notification(
            &mut server,
            "thread/realtime/transcript/done",
            json!({"threadId":"thread-1","role":"user","text":"hello"}),
        )
        .await;
        notification(
            &mut server,
            "thread/realtime/transcript/delta",
            json!({"threadId":"thread-1","role":"assistant","delta":"re"}),
        )
        .await;
        notification(
            &mut server,
            "thread/realtime/transcript/done",
            json!({"threadId":"thread-1","role":"assistant","text":"reply"}),
        )
        .await;
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
}

#[tokio::test(start_paused = true)]
async fn call_keeps_early_transcript_and_tool_request_before_sdp() {
    let (mut io, mut server) = fixture();
    let client = async {
        let mut call = Call::new(&mut io);
        call.start(&options(), vec![], "fixture-offer", deadline())
            .await
            .unwrap();
        assert!(matches!(call.next().await.unwrap(), Event::ReplyDone { text } if text == "early"));
        assert!(
            matches!(call.next().await.unwrap(), Event::Tool { id:RequestId::Text(id), call }
            if id == "tool-request" && call.call_id == "call-1")
        );
        call.stop(deadline()).await.unwrap();
    };
    let remote = async {
        thread(&mut server).await;
        let request = read(&mut server).await;
        write(&mut server, json!({"id":request["id"],"result":{}})).await;
        notification(
            &mut server,
            "thread/realtime/transcript/done",
            json!({"threadId":"thread-1","role":"assistant","text":"early"}),
        )
        .await;
        write(
            &mut server,
            json!({"id":"tool-request","method":"item/tool/call","params":{
                "threadId":"thread-1","turnId":"turn-1","callId":"call-1","tool":"tool_0","arguments":{"text":"hello"}
            }}),
        )
        .await;
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
async fn call_start_deadline_includes_sdp_wait_and_prevents_generation_reuse() {
    let (mut io, mut server) = fixture();
    let client = async {
        let mut call = Call::new(&mut io);
        assert_eq!(
            call.start(
                &Options {
                    effort: Some("low"),
                    ..options()
                },
                vec![],
                "fixture-offer",
                deadline()
            )
            .await,
            Err(Error::Deadline)
        );
    };
    let remote = async {
        thread(&mut server).await;
        let request = read(&mut server).await;
        write(&mut server, json!({"id":request["id"],"result":{}})).await;
        let mut line = String::new();
        assert_eq!(server.read_line(&mut line).await.unwrap(), 0);
    };
    tokio::join!(client, remote);
    assert_eq!(
        io.begin_request("account/read", &json!({})),
        Err(RpcError::Disconnected)
    );
}

#[tokio::test(start_paused = true)]
async fn call_cancellation_closes_generation_without_a_replayed_start() {
    let (mut io, mut server) = fixture();
    let client = async {
        let mut call = Call::new(&mut io);
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            call.start(&options(), vec![], "fixture-offer", deadline()),
        )
        .await;
        assert!(result.is_err());
        drop(call);
    };
    let remote = async {
        let request = read(&mut server).await;
        assert_eq!(request["method"], "thread/start");
        writeln!(std::io::stdout(), "observed thread/start: {request}").unwrap();
        let mut line = String::new();
        assert_eq!(server.read_line(&mut line).await.unwrap(), 0);
    };
    tokio::join!(client, remote);
    assert_eq!(
        io.begin_request("account/read", &json!({})),
        Err(RpcError::Disconnected)
    );
}

#[tokio::test(start_paused = true)]
async fn call_unknown_and_foreign_requests_are_answered_without_tools() {
    let (mut io, mut server) = fixture();
    let client = async {
        let mut call = Call::new(&mut io);
        call.start(&options(), vec![], "fixture-offer", deadline())
            .await
            .unwrap();
        assert!(matches!(call.next().await.unwrap(), Event::Other));
        assert!(matches!(call.next().await.unwrap(), Event::Other));
        call.stop(deadline()).await.unwrap();
    };
    let remote = async {
        start(&mut server).await;
        for (id, method, thread) in [
            ("unknown", "unknown/method", "thread-1"),
            ("foreign", "item/tool/call", "foreign"),
        ] {
            write(
                &mut server,
                json!({"id":id,"method":method,"params":{"threadId":thread}}),
            )
            .await;
        }
        for id in ["unknown", "foreign"] {
            assert_eq!(
                read(&mut server).await,
                json!({"id":id,"result":{"contentItems":[],"success":false}})
            );
        }
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
}

#[tokio::test(start_paused = true)]
async fn call_stop_requires_matching_closed_and_bounds_missing_confirmation() {
    let (mut io, mut server) = fixture();
    let client = async {
        let mut call = Call::new(&mut io);
        call.start(&options(), vec![], "fixture-offer", deadline())
            .await
            .unwrap();
        assert_eq!(call.stop(deadline()).await, Err(Error::Deadline));
    };
    let remote = async {
        start(&mut server).await;
        let request = read(&mut server).await;
        assert_eq!(request["method"], "thread/realtime/stop");
        write(&mut server, json!({"id":request["id"],"result":{}})).await;
        notification(&mut server, "thread/realtime/closed", json!({"threadId":"foreign"})).await;
        let mut line = String::new();
        assert_eq!(server.read_line(&mut line).await.unwrap(), 0);
    };
    tokio::join!(client, remote);
    assert_eq!(
        io.begin_request("account/read", &json!({})),
        Err(RpcError::Disconnected)
    );
}
