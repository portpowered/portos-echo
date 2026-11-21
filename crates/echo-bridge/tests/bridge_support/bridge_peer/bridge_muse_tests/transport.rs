//! T3 fault injection only at the authenticated external Muse HTTP boundary.
//! Uses the production bridge adapter against a controlled loopback TLS peer.
use super::*;

struct Peer {
    child: Process,
    trust: Muse,
    url: String,
}

impl Peer {
    async fn start(mode: &str) -> Self {
        let trust = Muse::start("subjectAltName=IP:127.0.0.1").await;
        let mut command = Command::new("python3");
        command
            .current_dir(trust.directory.path())
            .arg(root().join("crates/echo-bridge/tests/bridge_support/bridge_peer/bridge_muse_tests/tls_peer.py"))
            .arg(mode);
        let mut child = Process::spawn(&mut command);
        let port: u16 = child.line().await.parse().unwrap();
        let origin = "https://127.0.0.1";
        Self {
            child,
            trust,
            url: format!("{origin}:{port}"),
        }
    }

    async fn close(mut self, gets: usize, task: Value) {
        let requests: Value = serde_json::from_str(&self.child.command("stop").await).unwrap();
        let requests = requests.as_array().unwrap();
        assert_eq!(requests.len(), 1 + gets);
        assert_eq!(requests[0], json!({"method":"POST","path":"/v1/tasks","body":task}));
        for request in &requests[1..] {
            assert_eq!(request, &json!({"method":"GET","path":"/v1/tasks/task-pending/events"}));
        }
        self.child.finish(0).await;
        assert_eq!(self.trust.calls(), Vec::<Value>::new());
        self.trust.stop().await;
    }
}

#[tokio::test]
#[ignore = "T3 requires prebuilt Muse binaries, Linux, openssl and python3"]
async fn muse_invalid_events_and_interrupted_admission_fail_without_retry() {
    for mode in [
        "wrong_id",
        "invalid",
        "oversized",
        "unavailable",
        "interrupted",
        "duplicate",
        "stalled",
    ] {
        journey(mode).await;
    }
}

async fn journey(mode: &str) {
    let mut peer = Peer::start(mode).await;
    let config = echo_config::MuseConfig {
        url: peer.url.clone(),
        ca_file: peer.trust.path("server.der"),
        credential_file: peer.trust.path("muse-credential"),
    };
    let backend = echo_bridge::bridge_muse::Backend::open(&config, true).unwrap();
    let task = task("muse-transport");
    let expected = serde_json::to_value(&task).unwrap();
    let submitted = backend.submit(task).await;
    if mode == "interrupted" {
        assert_eq!(submitted, Err(AgentError::UnscriptedTask));
    } else {
        let id = submitted.unwrap();
        assert_eq!(id.to_string(), "task-pending");
        let next = if mode == "stalled" {
            tokio::join!(backend.next_event(&id), peer.child.command("waiting")).0
        } else {
            backend.next_event(&id).await
        };
        let event = if mode == "duplicate" {
            TaskEvent::Completed {
                task_id: id.clone(),
                summary: "Muse test job completed".into(),
            }
        } else {
            TaskEvent::Failed {
                task_id: id.clone(),
                retryable: false,
                message: "Muse is unavailable. Please try again.".into(),
            }
        };
        assert_eq!(next, Some(event), "{mode}");
        assert_eq!(backend.next_event(&id).await, None, "{mode}");
    }
    peer.close(usize::from(mode != "interrupted"), expected).await;
}
