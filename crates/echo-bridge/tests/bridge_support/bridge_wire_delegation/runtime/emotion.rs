//! T3 component: production application, local Live/Responses and real spoof MCP.
use super::*;
use tokio::io::{BufReader, Lines};
type Events = Lines<BufReader<TcpStream>>;

fn face() -> Value {
    json!({"state":"emotion","name":"happy","frame_ms":80,"frames":["(^_^)","(^o^)"]})
}

async fn screen(app: &Application) -> Events {
    let address = trace(&app.path())
        .into_iter()
        .find(|e| e["event"] == "screen.listening")
        .unwrap();
    let mut socket = bridge_wire_screen::stream(address["address"].as_str().unwrap()).await;
    assert_eq!(
        bridge_wire_screen::state(&mut socket).await,
        json!({"state":"ready","message":"Echo Live ready"})
    );
    socket
}

async fn animate(app: &mut Application, screen: &mut Events, id: &str) -> TcpStream {
    let mut request = app.request(id).await;
    let call = json!({"type":"function_call","call_id":"face-call","name":"tool_3","arguments":"{\"name\":\"happy\"}"});
    let body = sse(&call);
    request.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    let mut followup = app.model.accept().await.unwrap().0;
    let history = read(&mut followup).await.body;
    assert_eq!(history["input"][2], call);
    let output: Value = serde_json::from_str(history["input"][3]["output"].as_str().unwrap()).unwrap();
    assert_eq!(
        output,
        json!({"content":[{"type":"text","text":face().to_string()}],"is_error":false})
    );
    assert_eq!(bridge_wire_screen::state(screen).await, face());
    followup
}

async fn cancelled(app: &mut Application, screen: &mut Events) {
    app.live
        .events
        .send(json!({"type":"response.done","response":{"status":"cancelled"}}).to_string())
        .await
        .unwrap();
    assert_eq!(
        bridge_short::read_reply(&mut app.satellite).await.0["type"],
        "audio-stop"
    );
    bridge_short::send(&mut app.satellite, "played", json!({})).await;
    bridge_short::send(&mut app.satellite, "audio-stop", json!({})).await;
    assert_eq!(bridge_wire_screen::state(screen).await, json!({"state":"connected"}));
    assert_eq!(
        bridge_wire_screen::state(screen).await,
        json!({"state":"ready","message":"Echo Live ready"})
    );
}

#[tokio::test(start_paused = true)]
#[ignore = "T3 controlled application and loopback peers"]
async fn m6_repeated_delegation_executes_once_and_cancelled_animation_cannot_overwrite_replacement() {
    let clock = bridge_short::ReadyClock::new();
    let mut app = Application::idle_with_emotion(8, 60, true).await;
    let mut screen = screen(&app).await;
    app.wake().await;
    assert_eq!(
        bridge_wire_screen::state(&mut screen).await,
        json!({"state":"listening"})
    );
    assert_eq!(
        bridge_wire_screen::state(&mut screen).await,
        json!({"state":"connected"})
    );
    let mut old = animate(&mut app, &mut screen, "face").await;
    app.live.events.send(event("face", "client")).await.unwrap();
    respond(&mut old, "first face").await.unwrap();
    app.commentary("face", "first face").await;
    app.live.events.send(event("face", "client")).await.unwrap();
    // Same ordered Live channel: the refusal is a barrier after both duplicates.
    app.live.events.send(event("barrier", "responses")).await.unwrap();
    while !trace(&app.path()).iter().any(|e| e["event"] == "delegation.refused") {
        tokio::task::yield_now().await;
    }
    assert_no_request(&app.model).await;
    assert_eq!(app.spoof.snapshot().unwrap().calls[0].count, 1);
    cancelled(&mut app, &mut screen).await;
    tokio::time::resume();
    app.wake().await;
    assert_eq!(
        bridge_wire_screen::state(&mut screen).await,
        json!({"state":"listening"})
    );
    assert_eq!(
        bridge_wire_screen::state(&mut screen).await,
        json!({"state":"connected"})
    );
    // An abandoned expiry cannot overwrite the replacement run.
    elapse(Duration::from_millis(160)).await;
    let mut fresh = app.request("fresh").await;
    respond(&mut fresh, "fresh result").await.unwrap();
    app.commentary("fresh", "fresh result").await;
    app.live.events.send("synthetic.more_audio".into()).await.unwrap();
    let path = app.path();
    bridge_short::receive_short(&mut app.satellite, &app.live, &path, 32).await;
    assert_eq!(
        bridge_wire_screen::state(&mut screen).await,
        json!({"state":"speaking"})
    );
    bridge_short::send(&mut app.satellite, "played", json!({})).await;
    bridge_short::send(&mut app.satellite, "audio-stop", json!({})).await;
    assert_eq!(
        bridge_wire_screen::state(&mut screen).await,
        json!({"state":"ready","message":"Echo Live ready"})
    );
    assert_no_request(&app.model).await;
    assert_eq!(app.spoof.snapshot().unwrap().calls.len(), 1);
    assert_eq!(app.spoof.snapshot().unwrap().calls[0].count, 1);
    app.stop.send(true).unwrap();
    assert_eq!(app.worker.await.unwrap(), Ok(()));
    app.spoof.shutdown().await.unwrap();
    drop(clock);
}
