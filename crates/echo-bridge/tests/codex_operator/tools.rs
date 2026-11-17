//! T3 functional: production configured owner, controlled stdio/media, real MCP.
//! No real-pin model, WebRTC or milestone evidence is claimed here.
use super::*;
use echo_bridge::bridge_voice::Configured;
use echo_link::fake::{FakeConnector, Observer};
use echo_media::{DropCounts, MediaPeer, PcmFrame, PeerConfig, PeerState};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::watch;

struct Media {
    state: watch::Sender<PeerState>,
    closed: Arc<AtomicUsize>,
}
impl MediaPeer for Media {
    fn create(_: PeerConfig) -> impl Future<Output = Result<Self, echo_media::Error>> {
        std::future::ready(Err(echo_media::Error::InvalidConfig))
    }
    fn offer(&self) -> impl Future<Output = Result<String, echo_media::Error>> {
        std::future::ready(Ok("fixture-offer".into()))
    }
    fn apply_answer(&self, answer: &str) -> impl Future<Output = Result<(), echo_media::Error>> {
        assert_eq!(answer, "fixture-answer");
        self.state.send_replace(PeerState::Connected);
        std::future::ready(Ok(()))
    }
    fn states(&self) -> watch::Receiver<PeerState> {
        self.state.subscribe()
    }
    fn send(&self, _: PcmFrame) -> Result<(), echo_media::Error> {
        Ok(())
    }
    fn recv(&mut self) -> impl Future<Output = Result<Option<PcmFrame>, echo_media::Error>> {
        std::future::pending()
    }
    fn dropped_frames(&self) -> DropCounts {
        DropCounts::default()
    }
    fn close(&mut self) -> impl Future<Output = Result<(), echo_media::Error>> {
        self.closed.fetch_add(1, Ordering::SeqCst);
        self.state.send_replace(PeerState::Closed);
        std::future::ready(Ok(()))
    }
}

fn config(fixture: &Fixture, url: &str) -> echo_config::BridgeConfig {
    let mut config = echo_config::BridgeConfig::load(fixture.directory.path().join("bridge.toml")).unwrap();
    config.audit.log_dir = fixture.directory.path().join("audit");
    config.mcp.server[0].url = url.into();
    config.mcp.server[0].auth = echo_config::McpAuth::None;
    config.mcp.server[0].bearer_file = None;
    config.mcp.server[0].allow_tools = ["test.echo", "test.wait", "test.requires_approval", "ui.show_emotion"]
        .map(String::from)
        .to_vec();
    config
}

async fn result(fixture: &Fixture) -> Value {
    loop {
        if let Ok(bytes) = std::fs::read(fixture.home.join("tool-result"))
            && let Ok(result) = serde_json::from_slice(&bytes)
        {
            return result;
        }
        tokio::task::yield_now().await;
    }
}

async fn generation(fixture: &Fixture, config: echo_config::BridgeConfig, accepted: Option<bool>) -> Value {
    let observer = Observer::default();
    let closed = Arc::new(AtomicUsize::new(0));
    let (stop, stopping) = tokio::sync::oneshot::channel();
    let prepared = Configured::new(
        config,
        fixture.directory.path().join("tool-trace"),
        ([127, 0, 0, 1], 0).into(),
    )
    .unwrap();
    let work = prepared.execute(
        FakeConnector::new(observer.clone()),
        async || {
            Ok(Media {
                state: watch::channel(PeerState::New).0,
                closed: closed.clone(),
            })
        },
        vec![],
        async {
            stopping.await.unwrap();
        },
    );
    let inspect = async {
        let (_control, mut screen) = ready(fixture).await;
        observer.wake("tool fixture", vec![]);
        if let Some(accepted) = accepted {
            loop {
                let state = bridge_wire_screen::state(&mut screen).await;
                if state["state"] == "approval" {
                    approve(fixture, &state, accepted).await;
                    break;
                }
            }
        }
        let output = result(fixture).await;
        stop.send(()).unwrap();
        while !observer
            .commands()
            .iter()
            .any(|command| matches!(command, echo_link::fake::Command::Cut(_)))
        {
            tokio::task::yield_now().await;
        }
        observer.finish(Ok(()));
        output
    };
    let (outcome, output) = Box::pin(tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        tokio::join!(work, inspect)
    }))
    .await
    .unwrap();
    assert_eq!(outcome, Ok(()));
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    assert_eq!(observer.commands().last(), Some(&echo_link::fake::Command::Close));
    super::host::released(fixture).await;
    output
}

async fn ready(fixture: &Fixture) -> (tokio::net::TcpStream, Lines<BufReader<tokio::net::TcpStream>>) {
    let control = fixture.control().await;
    let mut screen = bridge_wire_screen::stream(&fixture.screen.to_string()).await;
    loop {
        let state = bridge_wire_screen::state(&mut screen).await;
        if state["state"] == "ready" && state["message"] == "Echo Live ready" {
            return (control, screen);
        }
    }
}

async fn approve(fixture: &Fixture, state: &Value, accepted: bool) {
    let action = json!({"action":if accepted { "approve" } else { "deny" }, "id":state["id"]});
    let mut stream = tokio::net::TcpStream::connect(fixture.screen).await.unwrap();
    let body = action.to_string();
    stream.write_all(format!("POST /actions?token=synthetic-screen-pair HTTP/1.1\r\nHost: local\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 204"), "{response}");
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_configured_tools_correlate_catalog_mcp_and_persist_no_replay() {
    let fixture = Fixture::new("chatgpt");
    std::fs::write(fixture.home.join("mode"), "tool-echo").unwrap();
    let spoof = echo_spoof_mcp::Spoof::start(echo_spoof_mcp::Options::default())
        .await
        .unwrap();
    let config = config(&fixture, spoof.url());
    let first = generation(&fixture, config.clone(), None).await;
    assert_eq!(
        first,
        json!({"contentItems":[{"type":"inputText","text":"[{\"text\":\"delegated echo\",\"type\":\"text\"}]"}],"success":true})
    );
    std::fs::remove_file(fixture.home.join("tool-result")).unwrap();
    std::fs::remove_file(fixture.home.join("peer-port")).unwrap();
    let second = generation(&fixture, config, None).await;
    assert_eq!(
        second,
        json!({"contentItems":[{"type":"inputText","text":"tool request denied"}],"success":false})
    );
    let snapshot = spoof.snapshot().unwrap();
    assert_eq!(
        snapshot
            .calls
            .iter()
            .map(|c| (c.name.as_str(), c.count))
            .collect::<Vec<_>>(),
        [("test.echo", 1)]
    );
    assert_eq!(snapshot.requests.iter().filter(|r| r.method == "tools/list").count(), 2);
    spoof.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_configured_tools_approval_denial_has_no_mcp_and_acceptance_has_one_attempt() {
    for accepted in [false, true] {
        let fixture = Fixture::new("chatgpt");
        std::fs::write(fixture.home.join("mode"), "tool-approval").unwrap();
        let spoof = echo_spoof_mcp::Spoof::start(echo_spoof_mcp::Options::default())
            .await
            .unwrap();
        let output = generation(&fixture, config(&fixture, spoof.url()), Some(accepted)).await;
        assert_eq!(output["success"], false);
        assert_eq!(spoof.snapshot().unwrap().calls.len(), usize::from(accepted));
        if accepted {
            assert!(output.to_string().contains("approval_required"));
        } else {
            assert!(output.to_string().contains("tool request denied"));
        }
        spoof.shutdown().await.unwrap();
    }
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_configured_tools_exhausted_call_admission_never_opens_media_or_model() {
    let fixture = Fixture::new("chatgpt");
    std::fs::write(fixture.home.join("mode"), "tool-echo").unwrap();
    let spoof = echo_spoof_mcp::Spoof::start(echo_spoof_mcp::Options::default())
        .await
        .unwrap();
    let mut config = config(&fixture, spoof.url());
    config.agent.daily_request_cap = 0;
    let observer = Observer::default();
    let prepared = Configured::new(
        config,
        fixture.directory.path().join("trace"),
        ([127, 0, 0, 1], 0).into(),
    )
    .unwrap();
    let work = prepared.execute(
        FakeConnector::new(observer.clone()),
        async || -> Result<Media, echo_bridge::Error> { unreachable!("over-cap admission cannot create a peer") },
        vec![],
        std::future::pending(),
    );
    let inspect = async {
        let (_control, _screen) = ready(&fixture).await;
        observer.wake("refused", vec![]);
    };
    let (result, ()) = Box::pin(tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        tokio::join!(work, inspect)
    }))
    .await
    .unwrap();
    assert_eq!(result, Err(echo_bridge::Error::Unavailable));
    assert_eq!(spoof.snapshot().unwrap(), echo_spoof_mcp::Snapshot::default());
    assert!(!fixture.home.join("thread-config").exists());
    assert!(!fixture.home.join("tool-result").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("last-method")).unwrap(),
        "account/read"
    );
    super::host::released(&fixture).await;
    spoof.shutdown().await.unwrap();
}
