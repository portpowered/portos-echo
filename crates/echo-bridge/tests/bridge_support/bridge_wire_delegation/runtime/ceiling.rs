//! T3 retained client through the bridge, Wyoming, Responses and spoof MCP loopback boundaries.
use super::*;

#[tokio::test]
#[ignore = "T3 controlled application and loopback peers"]
async fn bridge_wire_runtime_ceiling_abandons_model_and_next_wake_plays() {
    let mut app = Application::start(3, 90).await;
    let mut stalled = app.request("stalled").await;
    elapse(Duration::from_secs(61)).await;
    assert!(app.live.commentary.try_recv().is_err());
    assert!(!trace(&app.path()).iter().any(|e| e["event"] == "run.failed"));
    elapse(Duration::from_secs(29)).await;
    model_abandoned(&mut stalled).await;
    bridge_lifecycle::control(&mut app.satellite, "hassmic-flush").await;
    bridge_short::send(&mut app.satellite, "played", json!({})).await;
    bridge_short::send(&mut app.satellite, "audio-stop", json!({})).await;
    app.wait_event("run.failed").await;
    bridge_refusal::error(&mut app.satellite, false).await;
    trace_checks::resolved(&app.path(), "stalled", "cancelled", "unobserved");
    assert_no_request(&app.model).await;
    next_wake_plays(&mut app, &mut stalled).await;
    assert_eq!(
        handback_edges(&app.path()),
        vec![
            (json!("session.commentary.append"), json!("run-2")),
            (json!("reply.started"), json!("run-2")),
            (json!("reply.played"), json!("run-2")),
        ]
    );
    let entries = trace(&app.path());
    assert_eq!(
        entries.iter().find(|e| e["event"] == "run.failed").unwrap()["reason"],
        "session waiting ceiling reached"
    );
    assert_eq!(app.live.calls.load(Ordering::SeqCst), 2);
    assert!(app.live.commentary.try_recv().is_err());
    app.finish().await;
}

#[tokio::test]
#[ignore = "T3 controlled application and loopback peers"]
async fn bridge_wire_runtime_acknowledgement_plays_across_the_run_ceiling() {
    let mut app = Application::start(3, 90).await;
    let mut stalled = app.request("stalled").await;
    elapse(Duration::from_secs(89)).await;
    app.live.send_acknowledged("synthetic.more_audio".into()).await;
    read_pcm(&mut app, 1).await;
    elapse(Duration::from_secs(2)).await;
    read_pcm(&mut app, 31).await;
    model_abandoned(&mut stalled).await;
    app.live
        .send_acknowledged(json!({"type":"response.audio.done"}).to_string())
        .await;
    bridge_lifecycle::control(&mut app.satellite, "audio-stop").await;
    assert_eq!(
        handback_edges(&app.path()),
        vec![(json!("reply.started"), json!("run-1"))]
    );
    bridge_short::send(&mut app.satellite, "played", json!({})).await;
    bridge_short::send(&mut app.satellite, "audio-stop", json!({})).await;
    bridge_lifecycle::control(&mut app.satellite, "hassmic-end").await;
    let entries = trace(&app.path());
    let played = entries.iter().find(|e| e["event"] == "reply.played").unwrap();
    assert_eq!(played["run_id"], "run-1");
    assert_eq!(played["outcome"], "played");
    assert!(
        !entries
            .iter()
            .any(|e| matches!(e["event"].as_str(), Some("reply.cut" | "run.failed")))
    );
    trace_checks::resolved(&app.path(), "stalled", "cancelled", "unobserved");
    next_wake_plays(&mut app, &mut stalled).await;
    assert_eq!(
        handback_edges(&app.path()),
        vec![
            (json!("reply.started"), json!("run-1")),
            (json!("reply.played"), json!("run-1")),
            (json!("session.commentary.append"), json!("run-2")),
            (json!("reply.started"), json!("run-2")),
            (json!("reply.played"), json!("run-2")),
        ]
    );
    assert_eq!(app.live.calls.load(Ordering::SeqCst), 2);
    assert!(app.live.commentary.try_recv().is_err());
    app.finish().await;
}

#[tokio::test]
#[ignore = "T3 controlled application and loopback peers"]
async fn bridge_wire_runtime_twelve_calls_after_acknowledgement_play_on_the_same_run() {
    let mut app = Application::start(20, 90).await;
    app.live.send_acknowledged("synthetic.muse_audio".into()).await;
    read_pcm(&mut app, 1).await;
    let mut socket = app.request("slow").await;
    for index in 1..=12 {
        let id = format!("call-{index}");
        let text = format!("delegated echo {index}");
        let mut item = super::super::failures::tool(&id);
        item["arguments"] = json!(json!({"text":text}).to_string());
        respond_item(&mut socket, &item).await.unwrap();
        socket = app.model.accept().await.unwrap().0;
        let request = read(&mut socket).await;
        let output = &request.body["input"][index * 2 + 1];
        assert_eq!(output["type"], "function_call_output");
        assert_eq!(output["call_id"], id);
        assert_eq!(
            serde_json::from_str::<Value>(output["output"].as_str().unwrap()).unwrap(),
            json!({"content":[{"type":"text","text":text}],"is_error":false})
        );
    }
    elapse(Duration::from_secs(61)).await;
    assert!(app.live.commentary.try_recv().is_err());
    respond(&mut socket, "twelve successful calls").await.unwrap();
    app.commentary("slow", "twelve successful calls").await;
    app.live.events.send("synthetic.more_audio".into()).await.unwrap();
    // Consume the remaining acknowledgement frames before checking answer PCM.
    read_pcm(&mut app, 9).await;
    play(&mut app).await;
    assert_eq!(
        handback_edges(&app.path()),
        vec![
            (json!("reply.started"), json!("run-1")),
            (json!("session.commentary.append"), json!("run-1")),
            (json!("reply.played"), json!("run-1")),
        ]
    );
    assert_eq!(app.live.calls.load(Ordering::SeqCst), 1);
    assert_no_request(&app.model).await;
    app.finish_with_calls(12).await;
}

fn handback_edges(path: &Path) -> Vec<(Value, Value)> {
    trace(path)
        .iter()
        .filter(|e| {
            matches!(
                e["event"].as_str(),
                Some("session.commentary.append" | "reply.started" | "reply.played")
            )
        })
        .map(|e| (e["event"].clone(), e["run_id"].clone()))
        .collect()
}

async fn play(app: &mut Application) {
    let path = app.path();
    bridge_short::receive_short(&mut app.satellite, &app.live, &path, 32).await;
    bridge_short::send(&mut app.satellite, "played", json!({})).await;
    bridge_short::send(&mut app.satellite, "audio-stop", json!({})).await;
    bridge_lifecycle::control(&mut app.satellite, "hassmic-end").await;
}

async fn next_wake_plays(app: &mut Application, stalled: &mut TcpStream) {
    app.wake().await;
    let _ = respond(stalled, "late result").await;
    let mut fresh = app.request("fresh").await;
    respond(&mut fresh, "next wake answered").await.unwrap();
    app.commentary("fresh", "next wake answered").await;
    app.live.events.send("synthetic.more_audio".into()).await.unwrap();
    play(app).await;
}

async fn model_abandoned(stalled: &mut TcpStream) {
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), stalled.read(&mut [0]))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}

async fn read_pcm(app: &mut Application, frames: usize) {
    for _ in 0..frames {
        let (header, bytes) = bridge_short::read_reply(&mut app.satellite).await;
        assert_eq!(header["type"], "audio-chunk");
        assert_eq!(bytes, vec![0; 960]);
    }
}
