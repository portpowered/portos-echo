//! T3 controlled event boundary: a 204 followed by matching Progress/Completed.
use super::*;
use echo_agent_api::{AgentBackend, Task};
use echo_contracts::task::TaskEvent;

#[tokio::test]
#[ignore = "T3 requires prebuilt Muse binaries, Linux, openssl and python3"]
async fn muse_empty_poll_remains_pending_before_progress_and_terminal() {
    let muse = Muse::start("subjectAltName=IP:127.0.0.1").await;
    let mut command = Command::new("python3");
    command
        .current_dir(muse.directory.path())
        .arg(root().join("crates/echo-bridge/tests/bridge_support/bridge_peer/bridge_muse_tests/tls_peer.py"));
    let mut peer = Process::spawn(&mut command);
    let port: u16 = peer.line().await.parse().unwrap();
    let origin = "https://127.0.0.1";
    let config = echo_config::MuseConfig {
        url: format!("{origin}:{port}"),
        ca_file: muse.path("server.der"),
        credential_file: muse.path("muse-credential"),
    };
    let backend = echo_bridge::bridge_muse::Backend::open(&config, true).unwrap();
    let task: Task = serde_json::from_value(json!({"delegation_id":"muse-pending", "instructions":"test", "context":{}, "deadline_seconds":60, "max_tool_calls":8})).unwrap();
    let submitted = serde_json::to_value(&task).unwrap();
    let id = backend.submit(task).await.unwrap();
    assert_eq!(id.to_string(), "task-pending");
    assert_eq!(
        backend.next_event(&id).await,
        Some(TaskEvent::Progress {
            task_id: id.clone(),
            message: "Still working".into()
        })
    );
    assert_eq!(
        backend.next_event(&id).await,
        Some(TaskEvent::Completed {
            task_id: id.clone(),
            summary: "Muse test job completed".into()
        })
    );
    assert_eq!(backend.next_event(&id).await, None);
    let requests: Value = serde_json::from_str(&peer.command("stop").await).unwrap();
    assert_eq!(
        requests,
        json!([
            {"method":"POST","path":"/v1/tasks","body":submitted},
            {"method":"GET","path":"/v1/tasks/task-pending/events"},
            {"method":"GET","path":"/v1/tasks/task-pending/events"},
            {"method":"GET","path":"/v1/tasks/task-pending/events"}
        ])
    );
    peer.finish(0).await;
    assert_eq!(muse.calls(), Vec::<Value>::new());
    evidence(
        "muse pending: task_id=task-pending submits=1 polls=3 responses=204,Progress,Completed terminal=1 replay_requests=0",
    );
    muse.stop().await;
}
