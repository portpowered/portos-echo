//! T3 functional: continuous capture across real local Muse admission and failure.
use super::*;
use crate::bridge_refusal::continuous::{Input, progress, settings};
use std::sync::{Arc, atomic::Ordering};

#[tokio::test]
#[ignore = "T3 requires prebuilt Muse daemon/report binaries and openssl"]
async fn muse_continuously_ready_input_delivers_delegation_and_failed_callback() {
    let muse = Muse::start("subjectAltName=IP:127.0.0.1").await;
    let directory = tempfile::tempdir().unwrap();
    configure(directory.path(), 1, &muse, "");
    let config = echo_config::BridgeConfig::load(directory.path().join("bridge.toml")).unwrap();
    let model = bridge_wire_delegation::Model::start(false, true).await;
    let mut live = bridge_peer::Live::bind_frames(0).await;
    let mut options = settings(directory.path(), &live.url);
    options.muse = Some((config.agent, config.muse.unwrap()));
    options.model_url = Some(model.origin.clone());
    let input = Input::default();
    let (stop, shutdown) = tokio::sync::oneshot::channel();
    let runtime = tokio::spawn(echo_bridge::bridge_runtime::execute_controlled(
        options,
        echo_bridge::bridge_runtime::Control {
            utc_day: Arc::new(|| 0),
            shutdown,
        },
        input.clone(),
    ));
    input.wake();
    let before = progress(&live.microphone_frames, 0).await;
    live.send_acknowledged(bridge_wire_delegation::event("muse-d-1", "client"))
        .await;
    let path = directory.path().join("trace");
    let admitted = wait(&path, "muse.progress").await;
    let id = admitted["task_id"].as_str().unwrap();
    assert_eq!(admitted["delegation_id"], "muse-d-1");
    assert_eq!(admitted["run_id"], "run-1");
    let at_progress = live.microphone_frames.load(Ordering::SeqCst);
    let after = progress(&live.microphone_frames, at_progress).await;
    assert!(at_progress >= before);
    assert!(after > at_progress);
    assert_eq!(muse.calls().len(), 1);
    check_gadget(&muse.calls()[0], id);
    report_failure(&muse, &mut live, id).await;
    check_failure_trace(&path, id).await;
    assert_eq!(model.request_count(), 0);
    evidence(&format!(
        "continuous Muse: mic_before={before} mic_at_progress={at_progress} mic_after={after} submissions=1 failed=1 retryable=false commentary=1 model_requests=0"
    ));
    live.send_acknowledged(json!({"type":"error"}).to_string()).await;
    wait(&path, "run.failed").await;
    stop.send(true).unwrap();
    assert_eq!(runtime.await.unwrap(), Ok(()));
    assert_eq!(muse.calls().len(), 1);
    muse.stop().await;
}

async fn report_failure(muse: &Muse, live: &mut bridge_peer::Live, id: &str) {
    let output = muse.report_output(id, "failed", "Muse test job failed").await;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"accepted":true,"task_id":id})
    );
    assert_eq!(
        serde_json::from_str::<Value>(
            &tokio::time::timeout(Duration::from_secs(8), live.commentary.recv())
                .await
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        json!({"type":"session.commentary.append","delegation_id":"muse-d-1","text":"Muse test job failed"})
    );
    let duplicate = muse.report_output(id, "failed", "duplicate").await;
    assert_eq!(duplicate.status.code(), Some(1));
    assert!(live.commentary.try_recv().is_err());
}

async fn check_failure_trace(path: &Path, id: &str) {
    let terminal = wait(path, "muse.terminal").await;
    assert_eq!(terminal["status"], "failed");
    assert_eq!(terminal["retryable"], false);
    assert_eq!(terminal["task_id"], id);
    assert_eq!(trace(path).iter().filter(|e| e["event"] == "muse.terminal").count(), 1);
    assert_eq!(trace(path).iter().filter(|e| e["event"] == "tools/call").count(), 0);
}
