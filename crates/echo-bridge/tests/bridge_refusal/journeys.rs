use super::*;

async fn wait_run(path: &Path, run: &str, event: &str) {
    tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        loop {
            if trace(path)
                .iter()
                .any(|row| row["run_id"] == run && row["event"] == event)
            {
                return;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "T3 requires make hassmic-host and prebuilt echo-bridge"]
async fn bridge_refusal_live_error_before_pcm_then_same_connection_recovers() {
    let directory = tempfile::tempdir().unwrap();
    let mut host = Process::spawn(&mut host_script("crates/echo-bridge/tests/bridge_refusal/host.sh"));
    bridge_wire_screen::setup(directory.path(), host.line().await.parse().unwrap());
    let live = bridge_peer::Live::bind_frames(0).await;
    let mut bridge = Process::spawn(&mut bridge_wire_screen::run_command(directory.path(), &live.url));
    assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
    let path = directory.path().join("trace");
    assert_eq!(host.command("start").await, "listening");
    wait_run(&path, "run-1", "mic.frames").await;
    live.events.send(json!({"type":"error"}).to_string()).await.unwrap();
    let result: Value = serde_json::from_str(&host.command("result").await).unwrap();
    assert_refusal(&result, "unavailable", 1);
    bridge_wire_auth_failure::wait_event(&path, "run.failed").await;
    bridge_wire_auth_failure::assert_surfaces(&path, false).await;
    assert_eq!(host.command("start").await, "listening");
    wait_run(&path, "run-2", "mic.frames").await;
    live.events.send("synthetic.more_audio".into()).await.unwrap();
    wait_run(&path, "run-2", "reply.started").await;
    live.events
        .send(json!({"type":"response.audio.done"}).to_string())
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&bridge.line().await).unwrap()["run_id"],
        "run-2"
    );
    let result: Value = serde_json::from_str(&host.command("idle").await).unwrap();
    assert_eq!(result["connections"], 1);
    assert_eq!(result["disconnections"], 0);
    assert_eq!(result["errors"], json!([]));
    assert_eq!(result["pcm_bytes"], 30720);
    assert_eq!(live.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    bridge.finish(0).await;
    host.finish(0).await;
}

#[tokio::test]
#[ignore = "T3 requires make hassmic-host and prebuilt echo-bridge"]
async fn bridge_refusal_post_pcm_failure_preserves_host_cut_idle_without_error() {
    let directory = tempfile::tempdir().unwrap();
    let mut host = Process::spawn(&mut host_script("crates/echo-bridge/tests/bridge_refusal/host.sh"));
    bridge_wire_screen::setup(directory.path(), host.line().await.parse().unwrap());
    let live = bridge_peer::Live::bind_frames(3).await;
    let mut bridge = Process::spawn(&mut bridge_wire_screen::run_command(directory.path(), &live.url));
    assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
    let path = directory.path().join("trace");
    assert_eq!(host.command("start").await, "listening");
    wait_run(&path, "run-1", "reply.started").await;
    live.events.send(json!({"type":"error"}).to_string()).await.unwrap();
    bridge_wire_auth_failure::wait_event(&path, "run.failed").await;
    let result: Value = serde_json::from_str(&host.command("idle").await).unwrap();
    assert_eq!(result["errors"], json!([]));
    assert_eq!(result["connections"], 1);
    assert_eq!(result["disconnections"], 0);
    assert!(result["pcm_bytes"].as_u64().unwrap() > 0);
    writeln!(
        std::io::stdout(),
        "post_pcm_failure cut/played returned pinned host idle; no extra error: {result}"
    )
    .unwrap();
    bridge.finish(0).await;
    host.finish(0).await;
}
