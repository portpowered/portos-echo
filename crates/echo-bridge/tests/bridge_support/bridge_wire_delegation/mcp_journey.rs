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
        assert_eq!(bridge_wire_screen::state(&mut session.screen).await, expected);
        completed(
            &operator(directory.path(), "--refresh-now").await.unwrap(),
            1, "", "echo-bridge: Grant requires relinking\n",
        );
        assert_eq!(bridge_wire_screen::state(&mut session.screen).await, expected);
        reach(&mut session.process, &path, "mcp.run.failed").await.unwrap();
        next_wake.send(()).await.unwrap();
        reach(&mut session.process, &path, "delegation.refused").await.unwrap();
        session.process.finish(0).await;
        satellite.await.unwrap();
        rejected_once(&mock, &model, &path);
        assert_eq!(live.observer().calls().unwrap().len(), 2);
        live.shutdown().await;
    }).await;
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
    let refused: Vec<_> = events.iter().filter(|e| e["event"] == "delegation.refused").collect();
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0]["run_id"], "run-2");
    assert_eq!(refused[0]["reason"], "mcp_relink_required");
    assert_eq!(
        events
            .iter()
            .filter(|e| e["event"] == "session.commentary.append")
            .count(),
        0
    );
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

pub(super) async fn answer(socket: TcpStream, mut next_wake: tokio::sync::mpsc::Receiver<()>) {
    let (reader, mut writer) = socket.into_split();
    let mut lines = BufReader::new(reader).lines();
    loop {
        // next_line is cancellation safe when a wake arrives mid-header.
        let line = tokio::select! {
            line = lines.next_line() => match line.unwrap() { Some(line) => line, None => return },
            Some(()) = next_wake.recv() => { wake(&mut writer).await; continue; }
        };
        let header: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(header["payload_length"].as_u64().unwrap_or(0), 0);
        match header["type"].as_str().unwrap() {
            "describe" => bridge_short::send(&mut writer, "info", json!({"satellite":{"name":"synthetic"}})).await,

            "audio-stop" => {
                bridge_short::send(&mut writer, "played", json!({})).await;
                bridge_short::send(&mut writer, "audio-stop", json!({})).await;
            }
            _ => assert_eq!(header["type"], "audio-start"),
        }
    }
}
