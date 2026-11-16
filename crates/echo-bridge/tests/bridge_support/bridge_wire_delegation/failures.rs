//! Component T3: configured limits and an actual MCP HTTP 500, without replay.
use super::*;

pub(super) async fn scripted(items: Vec<Value>) -> Model {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = "http://127.0.0.1";
    let origin = format!("{base}:{}", listener.local_addr().unwrap().port());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = requests.clone();
    let worker = tokio::spawn(async move {
        for item in items {
            let mut socket = listener.accept().await.unwrap().0;
            let request = read(&mut socket).await;
            observed.lock().unwrap().push(request);
            let body = sse(&item);
            socket.write_all(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
            ).as_bytes()).await.unwrap();
        }
        // Keep the endpoint available: an unexpected replay must be observable.
        loop {
            let mut socket = listener.accept().await.unwrap().0;
            let request = read(&mut socket).await;
            observed.lock().unwrap().push(request);
            socket
                .write_all(b"HTTP/1.1 503 fixture\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        }
    });
    Model {
        origin,
        requests,
        worker,
    }
}

pub(super) fn tool(id: &str) -> Value {
    json!({"type":"function_call","call_id":id,"name":"tool_0","arguments":"{\"text\":\"delegated echo\"}"})
}

pub(super) fn answer(text: &str) -> Value {
    json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]})
}

async fn limits(tool_cap: bool) {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let spoof = Spoof::start(Options::default()).await.unwrap();
    configure(
        directory.path(),
        listener.local_addr().unwrap().port(),
        "api_key",
        &spoof,
        true,
    )
    .await;
    configure_cap(directory.path(), tool_cap);
    let model = scripted(vec![tool("call-1"), tool("call-2")]).await;
    let mut live = bridge_peer::Live::bind_frames(0).await;
    let mut bridge = Process::spawn(&mut run_command(directory.path(), &live.url, &model.origin));
    let mut satellite = listener.accept().await.unwrap().0;
    bridge_short::register(&mut satellite, &mut bridge).await;
    let mut satellite = bridge_short::Satellite::new(satellite);
    assert_eq!(bridge_short::read_reply(&mut satellite).await.0["type"], "audio-start");
    exercise_limits(&mut live, tool_cap).await;
    bridge.finish(0).await;
    check_limits(directory.path(), &model, &spoof, &live, tool_cap);
    spoof.shutdown().await.unwrap();
}

fn check_limits(directory: &Path, model: &Model, spoof: &Spoof, live: &bridge_peer::Live, tool_cap: bool) {
    assert_eq!(model.requests.lock().unwrap().len(), if tool_cap { 3 } else { 1 });
    assert_eq!(spoof.snapshot().unwrap().calls.len(), 1);
    let entries = trace(&directory.join("trace"));
    assert_eq!(
        entries
            .iter()
            .filter(|e| e["event"] == "session.commentary.append")
            .count(),
        2
    );
    assert_eq!(
        entries.iter().filter(|e| e["event"] == "auth.relink_required").count(),
        0
    );
    assert_eq!(live.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

async fn exercise_limits(live: &mut bridge_peer::Live, tool_cap: bool) {
    for id in ["limit-1", "limit-2"] {
        live.events.send(event(id, "client")).await.unwrap();
        let value = tokio::time::timeout(OBSERVATION_TIMEOUT, live.commentary.recv())
            .await
            .unwrap()
            .unwrap();
        let expected = limit_message(tool_cap, id);
        assert_eq!(
            serde_json::from_str::<Value>(&value).unwrap(),
            json!({
                "type":"session.commentary.append","delegation_id":id,"text":expected
            })
        );
    }
}

fn configure_cap(directory: &Path, tool_cap: bool) {
    let path = directory.join("bridge.toml");
    let (before, after) = if tool_cap {
        ("max_tool_calls = 8", "max_tool_calls = 1")
    } else {
        ("daily_request_cap = 500", "daily_request_cap = 1")
    };
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains(before));
    std::fs::write(path, content.replace(before, after)).unwrap();
}

fn limit_message(tool_cap: bool, id: &str) -> &'static str {
    match (tool_cap, id) {
        (true, "limit-1") => "tool call limit reached",
        (true, _) => "model request failed",
        (false, _) => "daily request limit reached",
    }
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_delegation_daily_cap_retained_across_tasks() {
    limits(false).await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_delegation_tool_cap_refuses_one_additional_tool() {
    limits(true).await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_wire_delegation_mcp_500_is_one_attempt_and_exact_error() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let spoof = Spoof::start(Options {
        fault: echo_spoof_mcp::Fault::Http500,
        ..Options::default()
    })
    .await
    .unwrap();
    configure(
        directory.path(),
        listener.local_addr().unwrap().port(),
        "api_key",
        &spoof,
        true,
    )
    .await;
    let model = scripted(vec![tool("call-1"), answer("tool failed")]).await;
    let fake = FakeLive::bind(FakeScript {
        events: vec![event("d-1", "client"), event("d-1", "client")],
        response: FakeResponse::Answer,
    })
    .await
    .unwrap();
    let origin = fake.base_url().trim_end_matches('/').to_owned();
    let mut bridge = Process::spawn(&mut run_command(directory.path(), &origin, &model.origin));
    let mut satellite = listener.accept().await.unwrap().0;
    bridge_short::register(&mut satellite, &mut bridge).await;
    let _satellite = bridge_short::Satellite::new(satellite);
    assert_eq!(
        commentary(&fake.observer()).await,
        json!({
            "type":"session.commentary.append","delegation_id":"d-1","text":"tool failed"
        })
    );
    let path = directory.path().join("trace");
    bridge_wire_auth_failure::wait_event(&path, "session.commentary.append").await;
    bridge.finish(0).await;
    {
        let requests = model.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let output: Value = serde_json::from_str(requests[1].body["input"][3]["output"].as_str().unwrap()).unwrap();
        assert_eq!(
            output,
            json!({"content":{"error":format!("server error: http_status:{}", 500)},"is_error":true})
        );
    }
    assert_eq!(spoof.snapshot().unwrap().calls.len(), 1);
    assert_eq!(fake.observer().calls().unwrap()[0].commentary.len(), 1);
    check_trace(&path, false, "synthetic-api-key");
    fake.shutdown().await;
    spoof.shutdown().await.unwrap();
}
