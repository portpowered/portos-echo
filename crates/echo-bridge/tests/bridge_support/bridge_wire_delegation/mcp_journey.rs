//! T3 delivered bridge: a revoked resource grant reaches the screen and stays latched.
use super::*;

#[tokio::test(start_paused = true)]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn mcp_link_resource_401_shows_relink_without_dead_token_retry() {
    drive(async {
        let directory = tempfile::tempdir().unwrap();
        let mock = Mock::bind().await.unwrap();
        let live = FakeLive::bind(FakeScript {
            events: vec![event("revoked", "client")],
            response: FakeResponse::Answer,
        }).await.unwrap();
        let model = Model::start(false, true).await;
        let mut session = Session::start_with(
            directory.path(), &mock, true,
            Some((live.base_url().trim_end_matches('/'), &model.origin)),
        ).await;
        link(&mut session, &mock, directory.path()).await;
        mock.state.lock().unwrap().revoked = true;
        wake(&mut session.satellite).await;
        let path = directory.path().join("trace");
        let (next_wake, wakes) = tokio::sync::mpsc::channel(1);
        let satellite = tokio::spawn(answer(session.satellite, wakes));
        reach(&mut session.process, &path, "mcp.relink_required").await.unwrap();
        let expected = json!({"state":"error","message":"Run echo-bridge link with your config to relink Port OS MCP.","relink":true});
        assert_eq!(bridge_wire_screen::state(&mut session.screen).await, json!({"state":"listening"}));
        assert_eq!(bridge_wire_screen::state(&mut session.screen).await, json!({"state":"connected"}));
        assert_eq!(bridge_wire_screen::state(&mut session.screen).await, expected);
        completed(
            &operator(directory.path(), "--refresh-now").await.unwrap(),
            1, "", "echo-bridge: Grant requires relinking\n",
        );
        assert_eq!(bridge_wire_screen::state(&mut session.screen).await, expected);
        // Once the 401 is observed, hold both clocks fixed. Completion and the
        // cut/Played handshake must wake themselves, before any deadline tick.
        failed_without_clock_advance(&path).await;
        next_wake.send(()).await.unwrap();
        // Refusal and its loopback handshakes need no elapsed product time.
        // Advancing here can expire a probe before instrumented I/O is serviced.
        event_without_clock_advance(&path, "delegation.refused").await;
        session.process.finish(0).await;
        assert_eq!(satellite.await.unwrap(), 1);
        rejected_once(&mock, &model, &path);
        assert_eq!(live.observer().calls().unwrap().len(), 2);
        live.shutdown().await;
    }).await;
}

async fn failed_without_clock_advance(path: &Path) {
    let events = event_without_clock_advance(path, "mcp.run.failed").await;
    let failed = events.iter().find(|e| e["event"] == "mcp.run.failed").unwrap();
    assert_eq!(failed["reason"], "relink_required");
    assert_eq!(events.iter().filter(|e| e["event"] == "run.failed").count(), 0);
    assert_eq!(
        events.iter().filter(|e| e["event"] == "wyoming.reconnecting").count(),
        0
    );
}

async fn event_without_clock_advance(path: &Path, event: &str) -> Vec<Value> {
    let started = std::time::Instant::now();
    loop {
        let text = tokio::fs::read_to_string(path).await.unwrap();
        let events: Vec<Value> = text
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        if events.iter().any(|e| e["event"] == event) {
            return events;
        }
        assert!(
            started.elapsed() < OBSERVATION_TIMEOUT,
            "did not observe {event} with clocks frozen: {events:?}"
        );
    }
}

pub(super) async fn link(session: &mut Session, mock: &Mock, directory: &Path) {
    let link = operator(directory, "link");
    session.prompt(mock).await;
    tokio::time::advance(Duration::from_secs(5)).await;
    session.advance(5000).await;
    completed(&link.await.unwrap(), 0, "MCP linked\n", "");
    assert_eq!(
        bridge_wire_screen::state(&mut session.screen).await,
        json!({"state":"ready","message":"Echo Live ready"})
    );
}

fn rejected_once(mock: &Mock, model: &Model, path: &Path) {
    let state = mock.state.lock().unwrap();
    let resources: Vec<_> = state.requests.iter().filter(|r| r.path == "/mcp").collect();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].headers["authorization"], "Bearer access-1");
    assert!(!resources[0].headers.contains_key("dpop"));
    assert_eq!(state.requests.iter().filter(|r| r.path == "/custom/link").count(), 1);
    assert_eq!(
        state
            .requests
            .iter()
            .filter(|r| r.form.get("grant_type").map(String::as_str) == Some("refresh_token"))
            .count(),
        0
    );
    assert_eq!(model.requests.lock().unwrap().len(), 0);
    let events = trace(path);
    let rejected: Vec<_> = events.iter().filter(|e| e["event"] == "mcp.relink_required").collect();
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0]["reason"], "unauthorized");
    assert_relink_chain(&events);
    assert_eq!(
        events.iter().filter(|e| e["event"] == "wyoming.reconnecting").count(),
        0
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e["event"] == "session.commentary.append")
            .count(),
        0
    );
}

fn assert_relink_chain(events: &[Value]) {
    let (failure_index, failed) = unique_journey_event(events, "mcp.run.failed");
    let (refusal_index, refused) = unique_journey_event(events, "delegation.refused");
    assert_eq!(failed["run_id"], "run-1");
    assert_eq!(failed["reason"], "relink_required");
    assert_eq!(refused["run_id"], "run-2");
    assert_eq!(refused["reason"], "mcp_relink_required");
    let starts: Vec<_> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| e["event"] == "run.started")
        .collect();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0].1["run_id"], "run-1");
    assert_eq!(starts[1].1["run_id"], "run-2");
    assert!(starts[0].0 < failure_index);
    assert!(failure_index < starts[1].0);
    assert!(starts[1].0 < refusal_index);
    assert_eq!(events.iter().filter(|e| e["event"] == "run.failed").count(), 0);
    let failure_ms = failed["elapsed_ms"].as_u64().unwrap();
    let refusal_ms = refused["elapsed_ms"].as_u64().unwrap();
    assert!(failure_ms <= refusal_ms);
    // reach can observe mcp.relink_required just after its check, leaving one
    // final acknowledged 20 ms advance after the failure sample. Both clocks
    // then stay frozen through run-2's refusal; this is no wall-time tolerance.
    assert!(refusal_ms - failure_ms <= 20);
}

fn unique_journey_event<'a>(events: &'a [Value], name: &str) -> (usize, &'a Value) {
    let matching: Vec<_> = events.iter().enumerate().filter(|(_, e)| e["event"] == name).collect();
    assert_eq!(matching.len(), 1, "expected exactly one {name}: {events:?}");
    matching[0]
}

pub(super) async fn reach(process: &mut Process, path: &Path, event: &str) -> Result<(), String> {
    // Drive both clocks while the persistent satellite services control traffic.
    // Thirty simulated seconds is a ceiling, never a synchronization sleep.
    for _ in 0..1500 {
        if trace(path).iter().any(|e| e["event"] == event) {
            return Ok(());
        }
        tokio::time::advance(Duration::from_millis(20)).await;
        let line = process.command(r#"{"action":"advance_clock","milliseconds":20}"#).await;
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap(),
            json!({"event":"bridge.clock_advanced","milliseconds":20})
        );
    }
    Err(format!("did not observe {event}: {:?}", trace(path)))
}

pub(super) async fn wake(writer: &mut (impl tokio::io::AsyncWrite + Unpin)) {
    bridge_short::send(
        writer,
        "run-pipeline",
        json!({
            "start_stage":"asr", "end_stage":"tts", "restart_on_end":false, "wake_word_name":"alexa"
        }),
    )
    .await;
}

pub(super) async fn answer(socket: TcpStream, mut next_wake: tokio::sync::mpsc::Receiver<()>) -> usize {
    let (reader, mut writer) = socket.into_split();
    let mut lines = BufReader::new(reader).lines();
    let mut errors = 0;
    loop {
        // next_line is cancellation safe when a wake arrives mid-header.
        let line = tokio::select! {
            line = lines.next_line() => match line.unwrap() { Some(line) => line, None => return errors },
            Some(()) = next_wake.recv() => { wake(&mut writer).await; continue; }
        };
        let header: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(header["payload_length"].as_u64().unwrap_or(0), 0);
        match header["type"].as_str().unwrap() {
            "describe" => bridge_short::send(&mut writer, "info", json!({"satellite":{"name":"synthetic"}})).await,
            "error" => {
                assert_eq!(
                    header["data"],
                    json!({"code":"echo-unavailable","text":"Live unavailable."})
                );
                errors += 1;
            }
            "audio-stop" => {
                bridge_short::send(&mut writer, "played", json!({})).await;
                bridge_short::send(&mut writer, "audio-stop", json!({})).await;
            }
            _ => assert_eq!(header["type"], "audio-start"),
        }
    }
}
