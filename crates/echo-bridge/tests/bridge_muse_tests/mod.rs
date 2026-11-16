//! T3: real selected bridge/daemon/report binaries and pinned playback host.
use super::*;
mod fixture;
mod pending;
use fixture::{Muse, configure};

fn evidence(message: &str) {
    use std::io::Write;
    writeln!(std::io::stdout(), "{message}").unwrap();
}

fn san(fault: &str) -> &'static str {
    if fault == "hostname" {
        "subjectAltName=DNS:localhost"
    } else {
        "subjectAltName=IP:127.0.0.1"
    }
}

async fn wait(path: &Path, event: &str) -> Value {
    let result = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(value) = trace(path).into_iter().find(|e| e["event"] == event) {
                return value;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await;
    assert!(result.is_ok(), "waiting for {event}: {:?}", trace(path));
    result.unwrap()
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host and prebuilt bridge/Muse binaries; Linux and openssl"]
async fn muse_correlated_success() {
    let muse = Muse::start("subjectAltName=IP:127.0.0.1").await;
    let directory = tempfile::tempdir().unwrap();
    let mut host = Process::spawn(&mut host());
    let port = host.line().await.parse().unwrap();
    configure(directory.path(), port, &muse, "");
    let model = bridge_wire_delegation::Model::start(false, true).await;
    let mut live = bridge_peer::Live::bind_frames(0).await;
    let mut bridge = Process::spawn(&mut bridge_wire_delegation::run_command(
        directory.path(),
        &live.url,
        &model.origin,
    ));
    assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
    assert_eq!(host.command("wake").await, "wake");
    let path = directory.path().join("trace");
    wait(&path, "mic.frames").await;
    live.events
        .send(bridge_wire_delegation::event("muse-d-1", "client"))
        .await
        .unwrap();
    let progress = wait(&path, "muse.progress").await;
    let id = progress["task_id"].as_str().unwrap();
    assert_eq!(progress["delegation_id"], "muse-d-1");
    assert_eq!(progress["run_id"], "run-1");
    assert!(live.commentary.try_recv().is_err()); // Chat output never implies success.
    let calls = muse.calls();
    assert_eq!(calls.len(), 1);
    check_gadget(&calls[0], id);
    assert_eq!(muse.report(id).await, json!({"accepted":true,"task_id":id}));
    reply(&mut live, &mut bridge, &mut host, &path, "Muse test job completed").await;
    check_trace(&path, Some(id));
    assert_eq!(model.request_count(), 0);
    bridge.finish(0).await;
    host.finish(0).await;
    assert_eq!(muse.calls().len(), 1);
    evidence(&format!(
        "muse success: delegation_id=muse-d-1 task_id={id} gadget_session={} report_task_id={id} submissions=1 progress=1 completed=1 commentary=1 reply_frames=10 pcm_bytes=9600 model_requests=0",
        calls[0]["argv"][2]
    ));
    muse.stop().await;
}

fn check_gadget(call: &Value, id: &str) {
    let argv = call["argv"].as_array().unwrap();
    assert_eq!(argv.len(), 6);
    assert_eq!(argv[0], "send-user-msg");
    assert_eq!(argv[1], "--session-id");
    let session = argv[2].as_str().unwrap();
    assert_eq!(session.len(), 36);
    assert_eq!(&session[14..15], "4");
    assert_eq!(argv[3], "--wait");
    assert_eq!(argv[4], "1");
    let event: Value = serde_json::from_str(&bridge_wire_delegation::event("muse-d-1", "client")).unwrap();
    assert_eq!(
        argv[5],
        format!(
            "Execute the delegated request and report its result.\nContext: {event}\nTask ID: {id}\nWhen you are finished, call the echo.report.result command with task_id {id}, status completed or failed, and a short summary. Do not report success any other way."
        )
    );
    assert_eq!(call["stdin"], "");
}

async fn reply(live: &mut bridge_peer::Live, bridge: &mut Process, host: &mut Process, path: &Path, text: &str) {
    let commentary = tokio::time::timeout(OBSERVATION_TIMEOUT, live.commentary.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&commentary).unwrap(),
        json!({"type":"session.commentary.append","delegation_id":"muse-d-1","text":text})
    );
    live.events.send("synthetic.muse_audio".into()).await.unwrap();
    wait(path, "reply.started").await;
    live.events
        .send(json!({"type":"response.audio.done"}).to_string())
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&bridge.line().await).unwrap(),
        json!({"event":"bridge.run_completed","run_id":"run-1"})
    );
    assert_eq!(host.command("pcm").await, "9600 0");
    assert!(live.commentary.try_recv().is_err());
}

fn check_trace(path: &Path, id: Option<&str>) {
    let entries = trace(path);
    for event in [
        "session.delegation.created",
        "session.commentary.append",
        "reply.started",
        "reply.played",
    ] {
        assert_eq!(entries.iter().filter(|e| e["event"] == event).count(), 1, "{event}");
    }
    for event in ["muse.submit", "muse.progress", "muse.terminal"] {
        let found: Vec<_> = entries.iter().filter(|e| e["event"] == event).collect();
        assert_eq!(found.len(), usize::from(id.is_some()), "{event}");
        if let Some(id) = id {
            assert_eq!(found[0]["task_id"], id);
            assert_eq!(found[0]["delegation_id"], "muse-d-1");
            assert_eq!(found[0]["run_id"], "run-1");
            if event == "muse.terminal" {
                assert_eq!(found[0]["status"], "completed");
            }
        }
    }
    assert_eq!(entries.iter().filter(|e| e["event"] == "tools/call").count(), 0);
    let raw = std::fs::read_to_string(path).unwrap();
    for private in [
        "synthetic-api-key",
        "Muse test job completed",
        "Done!",
        "server.der",
        "muse-credential",
    ] {
        assert!(!raw.contains(private), "private trace disclosure");
    }
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host and prebuilt bridge/Muse binaries; Linux and openssl"]
async fn muse_transport_refuses_without_client_fallback() {
    for fault in ["unreachable", "untrusted", "hostname", "authentication"] {
        transport_refusal(fault).await;
    }
}

async fn transport_refusal(fault: &str) {
    let muse = Muse::start(san(fault)).await;
    let directory = tempfile::tempdir().unwrap();
    let mut host = Process::spawn(&mut host());
    let port = host.line().await.parse().unwrap();
    configure(directory.path(), port, &muse, fault);
    let model = bridge_wire_delegation::Model::start(false, true).await;
    let mut live = bridge_peer::Live::bind_frames(0).await;
    let mut bridge = Process::spawn(&mut bridge_wire_delegation::run_command(
        directory.path(),
        &live.url,
        &model.origin,
    ));
    assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
    assert_eq!(host.command("wake").await, "wake");
    let path = directory.path().join("trace");
    wait(&path, "mic.frames").await;
    live.events
        .send(bridge_wire_delegation::event("muse-d-1", "client"))
        .await
        .unwrap();
    reply(
        &mut live,
        &mut bridge,
        &mut host,
        &path,
        "Muse is unavailable. Please try again.",
    )
    .await;
    check_trace(&path, None);
    no_execution(&model, &muse);
    bridge.finish(0).await;
    host.finish(0).await;
    evidence(&format!(
        "muse {fault}: admissions=0 gadget_calls=0 model_requests=0 commentary=1 reply_frames=10 pcm_bytes=9600"
    ));
    muse.stop().await;
}

fn no_execution(model: &bridge_wire_delegation::Model, muse: &Muse) {
    assert_eq!(model.request_count(), 0);
    assert_eq!(muse.calls(), Vec::<Value>::new());
}
