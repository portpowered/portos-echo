//! T3 Muse transport tests against the real daemon or a controlled TLS peer.
use super::*;
mod failures;
mod fixture;
mod pending;
mod transport;
use echo_agent_api::{AgentBackend, AgentError, Task};
use echo_contracts::{
    ids::{DelegationId, TaskId},
    task::TaskEvent,
};
use fixture::Muse;

fn evidence(message: &str) {
    use std::io::Write;
    writeln!(std::io::stdout(), "{message}").unwrap();
}

fn task(delegation: &str) -> Task {
    Task {
        delegation_id: DelegationId::new(delegation).unwrap(),
        instructions: "Run the test job".into(),
        context: Value::Null,
        deadline_seconds: 8,
        max_tool_calls: 8,
    }
}

async fn submit(backend: &echo_bridge::bridge_muse::Backend, delegation: &str) -> TaskId {
    let id = backend.submit(task(delegation)).await.unwrap();
    assert_eq!(
        backend.next_event(&id).await,
        Some(TaskEvent::Progress {
            task_id: id.clone(),
            message: "Done! The job completed successfully.".into(),
        })
    );
    id
}

fn peer(muse: &Muse) -> echo_bridge::bridge_muse::Backend {
    let config = fixture::config(muse.directory.path(), muse, "");
    echo_bridge::bridge_muse::Backend::open(&config, true).unwrap()
}

#[tokio::test]
#[ignore = "T3 requires prebuilt Muse binaries, Linux and openssl"]
async fn muse_correlated_success() {
    let muse = Muse::start("subjectAltName=IP:127.0.0.1").await;
    let backend = peer(&muse);
    let delegation = "muse-d-1";
    let id = submit(&backend, delegation).await;
    let calls = muse.calls();
    assert_eq!(calls.len(), 1);
    let argv = calls[0]["argv"].as_array().unwrap();
    assert_eq!(argv.len(), 6);
    assert_eq!(&argv[..2], &[json!("send-user-msg"), json!("--session-id")]);
    assert_eq!(&argv[3..5], &[json!("--wait"), json!("1")]);
    assert_eq!(calls[0]["stdin"], "");
    let session = argv[2].as_str().unwrap();
    assert_eq!(session.len(), 36);
    assert_eq!(&session[14..15], "4");
    assert_eq!(
        argv[5],
        format!(
            "Run the test job\nContext: null\nTask ID: {id}\nWhen you are finished, call the echo.report.result command with task_id {id}, status completed or failed, and a short summary. Do not report success any other way."
        )
    );
    assert_eq!(
        muse.report(&id.to_string()).await,
        json!({"accepted":true,"task_id":id})
    );
    assert_eq!(
        backend.next_event(&id).await,
        Some(TaskEvent::Completed {
            task_id: id.clone(),
            summary: "Muse test job completed".into(),
        })
    );
    assert_eq!(backend.next_event(&id).await, None);
    assert_eq!(muse.calls(), calls);
    evidence(&format!(
        "muse TLS success: delegation_id={delegation} task_id={id} session_id={session} submissions=1 progress=1 completed=1 callbacks=1"
    ));
    muse.stop().await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt Muse binaries, Linux and openssl"]
async fn muse_transport_refuses_unreachable_untrusted_wrong_hostname_and_bad_credential() {
    for fault in ["unreachable", "untrusted", "hostname", "authentication"] {
        let san = if fault == "hostname" {
            "subjectAltName=DNS:localhost"
        } else {
            "subjectAltName=IP:127.0.0.1"
        };
        let muse = Muse::start(san).await;
        let config = fixture::config(muse.directory.path(), &muse, fault);
        let backend = echo_bridge::bridge_muse::Backend::open(&config, true).unwrap();
        assert_eq!(
            backend.submit(task("muse-refused")).await,
            Err(AgentError::UnscriptedTask),
            "{fault}"
        );
        assert_eq!(muse.calls(), Vec::<Value>::new(), "{fault}");
        muse.stop().await;
    }
}
