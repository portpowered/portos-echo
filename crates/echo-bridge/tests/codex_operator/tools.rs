//! T3 functional: production configured owner, controlled stdio/media, real MCP.
//! No real-pin model, WebRTC or milestone evidence is claimed here.
use super::*;
use echo_bridge::bridge_voice::Configured;
use echo_link::fake::{FakeConnector, Observer};
use echo_media::{DropCounts, MediaPeer, PcmFrame, PeerConfig, PeerState, SetupObserver, SetupStage};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::watch;

mod trace_exits;
mod trace_failures;
mod trace_isolation;

pub(super) struct Media {
    pub state: watch::Sender<PeerState>,
    pub closed: Arc<AtomicUsize>,
    pub setup: OnceLock<SetupObserver>,
    /// Notified to make the next media read fail, as a lost call does.
    pub lost: Arc<tokio::sync::Notify>,
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
        std::future::ready((|| {
            if let Some(observer) = self.setup.get() {
                observer(SetupStage::IceConnected)?;
                observer(SetupStage::DataChannelOpen)?;
            }
            self.state.send_replace(PeerState::Connected);
            Ok(())
        })())
    }
    fn states(&self) -> watch::Receiver<PeerState> {
        self.state.subscribe()
    }
    fn observe_setup(&self, observer: SetupObserver) -> Result<(), echo_media::Error> {
        self.setup.set(observer).map_err(|_| echo_media::Error::State)
    }
    fn send(&self, _: PcmFrame) -> Result<(), echo_media::Error> {
        Ok(())
    }
    async fn recv(&mut self) -> Result<Option<PcmFrame>, echo_media::Error> {
        self.lost.notified().await;
        Err(echo_media::Error::State)
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

pub(super) fn config(fixture: &Fixture, url: &str) -> echo_config::BridgeConfig {
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

pub(super) async fn result(fixture: &Fixture) -> Value {
    loop {
        if let Ok(bytes) = std::fs::read(fixture.home.join("tool-result"))
            && let Ok(result) = serde_json::from_slice(&bytes)
        {
            return result;
        }
        tokio::task::yield_now().await;
    }
}

fn media(closed: &Arc<AtomicUsize>) -> Media {
    Media {
        state: watch::channel(PeerState::New).0,
        closed: closed.clone(),
        setup: OnceLock::new(),
        lost: Arc::default(),
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
        async || Ok(media(&closed)),
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
        wait_resolution(fixture).await;
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

pub(super) async fn ready(fixture: &Fixture) -> (tokio::net::TcpStream, Lines<BufReader<tokio::net::TcpStream>>) {
    let control = fixture.control().await;
    let mut screen = bridge_wire_screen::stream(&fixture.screen.to_string()).await;
    loop {
        let state = bridge_wire_screen::state(&mut screen).await;
        if state["state"] == "ready" && state["message"] == "Echo Live ready" {
            return (control, screen);
        }
    }
}

async fn wait_resolution(fixture: &Fixture) {
    if !std::fs::read_to_string(fixture.home.join("mode"))
        .unwrap()
        .starts_with("tool-trace")
    {
        return;
    }
    loop {
        let raw = std::fs::read_to_string(fixture.directory.path().join("tool-trace")).unwrap();
        let expected = if std::fs::read_to_string(fixture.home.join("mode")).unwrap() == "tool-trace-isolation" {
            2
        } else {
            1
        };
        if raw
            .lines()
            .filter(|line| serde_json::from_str::<Value>(line).is_ok_and(|row| row["event"] == "delegation.resolved"))
            .count()
            == expected
        {
            return;
        }
        tokio::task::yield_now().await;
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
    assert_eq!(
        receipts(&fixture),
        vec![
            receipt("item/tool/call", "test.echo"),
            receipt("tools/call", "test.echo")
        ]
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
    assert_eq!(
        receipts(&fixture),
        vec![
            receipt("item/tool/call", "test.echo"),
            receipt("tools/call", "test.echo"),
            receipt("item/tool/call", "test.echo")
        ]
    );
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
        let mut expected = vec![receipt("item/tool/call", "test.requires_approval")];
        if accepted {
            expected.push(receipt("tools/call", "test.requires_approval"));
        }
        assert_eq!(receipts(&fixture), expected);
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
    let (stop, stopping) = tokio::sync::oneshot::channel();
    let work = prepared.execute(
        FakeConnector::new(observer.clone()),
        async || -> Result<Media, echo_bridge::Error> { unreachable!("over-cap admission cannot create a peer") },
        vec![],
        async {
            stopping.await.unwrap();
        },
    );
    let inspect = async {
        let (_control, _screen) = ready(&fixture).await;
        observer.wake("refused", vec![]);
        // The refused wake ends on the device and the bridge stays up.
        let refusal = echo_link::fake::Command::FailRun(echo_link::RunFailure::Unavailable);
        while !observer.commands().contains(&refusal) {
            tokio::task::yield_now().await;
        }
        stop.send(()).unwrap();
    };
    let (result, ()) = Box::pin(tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        tokio::join!(work, inspect)
    }))
    .await
    .unwrap();
    assert_eq!(result, Ok(()));
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

fn receipts(fixture: &Fixture) -> Vec<Value> {
    let raw = std::fs::read_to_string(fixture.directory.path().join("tool-trace")).unwrap();
    assert!(!raw.contains("delegated echo"), "tool arguments leaked into trace");
    raw.lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| matches!(row["event"].as_str(), Some("item/tool/call" | "tools/call")))
        .map(|mut row| {
            assert!(row["elapsed_ms"].is_u64());
            row["elapsed_ms"] = json!(0);
            row
        })
        .collect()
}

fn receipt(event: &str, name: &str) -> Value {
    let mut value = json!({"event":event,"name":name,"thread_id":"fixture-thread","turn_id":"fixture-turn",
        "call_id":"fixture-call","run_id":"run-1","elapsed_ms":0,"provider_auth_mode":"chatgpt"});
    if event == "item/tool/call" {
        value["request_id"] = json!("fixture-tool");
    }
    value
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn delegation_trace_configured_search_and_mcp_complete_once_with_payload_switch() {
    for (payloads, mode) in [
        (true, "tool-trace"),
        (false, "tool-trace"),
        (true, "tool-trace-early"),
        (false, "tool-trace-early"),
    ] {
        let fixture = Fixture::new("chatgpt");
        std::fs::write(fixture.home.join("mode"), mode).unwrap();
        let spoof = echo_spoof_mcp::Spoof::start(echo_spoof_mcp::Options::default())
            .await
            .unwrap();
        let mut config = config(&fixture, spoof.url());
        config.mcp.server[0].name = "spoof-trace".into();
        config.trace.payloads = payloads;
        let output = generation(&fixture, config, None).await;
        assert_eq!(output["success"], true);
        assert_eq!(
            spoof
                .snapshot()
                .unwrap()
                .calls
                .iter()
                .map(|call| call.count)
                .sum::<u64>(),
            1
        );
        let raw = std::fs::read_to_string(fixture.directory.path().join("tool-trace")).unwrap();
        let mut rows: Vec<Value> = raw
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .filter(|row: &Value| {
                row["event"]
                    .as_str()
                    .is_some_and(|event| event.starts_with("delegation."))
            })
            .collect();
        assert_eq!(rows.len(), 4);
        rows[1..3].sort_by_key(|row| row["kind"] != "web_search");
        assert_eq!(rows[0]["event"], "delegation.created");
        assert_eq!(rows[1]["kind"], "web_search");
        assert_eq!(rows[1]["name"], "web_search");
        assert_eq!(rows[1]["result_count"], 1);
        assert_eq!(rows[2]["kind"], "mcp");
        assert_eq!(rows[2]["name"], "test.echo");
        assert_eq!(rows[2]["server"], "spoof-trace");
        assert_eq!(rows[2]["call_id"], "fixture-call");
        assert_eq!(rows[2]["request_id"], "fixture-tool");
        assert_trace_metadata(&rows);
        assert_trace_payloads(&raw, &rows, payloads);
        for row in rows {
            use std::io::Write;
            writeln!(std::io::stdout(), "{row}").unwrap();
        }
        spoof.shutdown().await.unwrap();
    }
}

fn assert_trace_metadata(rows: &[Value]) {
    for row in &rows[1..3] {
        assert_eq!(row["status"], "completed");
        assert!(row["duration_ms"].is_u64());
        assert_eq!(row["turn_id"], "fixture-turn");
        assert_eq!(row["thread_id"], "fixture-thread");
    }
    assert_eq!(rows[3]["event"], "delegation.resolved");
    assert_eq!(rows[3]["outcome"], "completed");
    assert_eq!(rows[3]["tool_counts"], json!({"mcp":1,"web_search":1}));
    assert_eq!(rows[3]["handback"], "unobserved");
}

fn assert_trace_payloads(raw: &str, rows: &[Value], payloads: bool) {
    assert!(!raw.contains("synthetic-account"));
    assert!(!raw.contains("synthetic-auth"));
    for seed in [
        "synthetic-query-7",
        "http://127.0.0.1/synthetic-source",
        "synthetic-result-7",
        "delegated echo",
    ] {
        assert_eq!(raw.contains(seed), payloads, "{seed}");
    }
    if payloads {
        assert_eq!(rows[1]["response"]["results"][0]["account_id"], "[redacted]");
        assert_eq!(rows[2]["request"], json!({"text":"delegated echo"}));
        assert_eq!(rows[3]["answer"], json!({"text":"synthetic-result-7"}));
    } else {
        for row in rows {
            for key in ["task", "request", "response", "answer"] {
                assert!(row.get(key).is_none());
            }
        }
    }
}
