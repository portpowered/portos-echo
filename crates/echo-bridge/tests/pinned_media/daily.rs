//! T3: configured production policy/budget with the real pin and loopback MCP/model.
//! Device and media are controlled ports: this isolates admission, not acoustics.
use super::*;
use echo_bridge::bridge_voice::Configured;
use echo_link::fake::{Command as LinkCommand, FakeConnector, Observer};
use echo_media::{PcmFrame, PeerState, SetupObserver, SetupStage};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::{oneshot, watch};

type Lines = tokio::io::Lines<BufReader<tokio::process::ChildStdout>>;
struct Provider {
    child: tokio::process::Child,
    input: tokio::process::ChildStdin,
    output: Lines,
    origin: String,
    model_ingress: usize,
}
impl Provider {
    async fn start() -> Self {
        let mut child = Command::new("python3")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pinned_media/provider.py"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut this = Self {
            child,
            input,
            output,
            origin: String::new(),
            model_ingress: 0,
        };
        this.origin = this.read().await["origin"].as_str().unwrap().to_owned();
        this
    }
    async fn read(&mut self) -> Value {
        serde_json::from_str(&self.output.next_line().await.unwrap().expect("provider EOF")).unwrap()
    }
    async fn send(&mut self, value: Value) {
        self.input.write_all(format!("{value}\n").as_bytes()).await.unwrap();
    }
    async fn cycle(&mut self, identity: &str, exhausted: bool) {
        let mut tools: Vec<_> = echo_spoof_mcp::catalog().as_array().unwrap().iter().enumerate()
            .map(|(i, t)| json!({"name":(["test_echo", "test_wait", "test_requires_approval", "ui_show_emotion"][i]),"description":t["description"],"inputSchema":t["inputSchema"]})).collect();
        // The configured bridge appends its two session builtins after discovery.
        tools.extend(session_tools());
        self.model_ingress = 0;
        self.send(json!({"tools":tools,"negative":false,"exhausted":exhausted,"manual":true,"identity":identity}))
            .await;
        assert_eq!(self.read().await, json!({"initialized":identity}));
        self.send(json!({"transcripts":true})).await;
    }
    async fn snapshot(&mut self) -> Value {
        self.send(json!({"snapshot":true})).await;
        loop {
            let value = self.read().await;
            if value == json!({"model_started":true}) {
                self.model_ingress += 1;
            } else {
                return value;
            }
        }
    }

    async fn close(mut self) {
        drop(self.input);
        assert!(self.child.wait().await.unwrap().success());
    }
}

// SDP is opaque to the app server. This controlled peer makes no network calls;
// the independently retained M4 witness owns real ICE/DTLS/Opus evidence.
struct Media(watch::Sender<PeerState>, OnceLock<SetupObserver>);
impl MediaPeer for Media {
    fn create(_: PeerConfig) -> impl Future<Output = Result<Self, echo_media::Error>> {
        std::future::ready(Ok(Self(watch::channel(PeerState::New).0, OnceLock::new())))
    }
    fn offer(&self) -> impl Future<Output = Result<String, echo_media::Error>> {
        std::future::ready(Ok("fixture-offer".into()))
    }
    fn apply_answer(&self, answer: &str) -> impl Future<Output = Result<(), echo_media::Error>> {
        assert_eq!(answer, "fixture-answer");
        std::future::ready((|| {
            if let Some(observer) = self.1.get() {
                observer(SetupStage::IceConnected)?;
                observer(SetupStage::DataChannelOpen)?;
            }
            self.0.send_replace(PeerState::Connected);
            Ok(())
        })())
    }
    fn states(&self) -> watch::Receiver<PeerState> {
        self.0.subscribe()
    }
    fn observe_setup(&self, observer: SetupObserver) -> Result<(), echo_media::Error> {
        self.1.set(observer).map_err(|_| echo_media::Error::State)
    }
    fn send(&self, _: PcmFrame) -> Result<(), echo_media::Error> {
        Ok(())
    }
    async fn recv(&mut self) -> Result<Option<PcmFrame>, echo_media::Error> {
        std::future::pending().await
    }
    fn dropped_frames(&self) -> DropCounts {
        DropCounts::default()
    }
    fn close(&mut self) -> impl Future<Output = Result<(), echo_media::Error>> {
        self.0.send_replace(PeerState::Closed);
        std::future::ready(Ok(()))
    }
}

fn configure(root: &Path, spoof: &echo_spoof_mcp::Spoof, cap: u64) -> echo_config::BridgeConfig {
    voice::configure(root, 16700);
    let mut config = echo_config::BridgeConfig::load(root.join("bridge.toml")).unwrap();
    config.live.credential_file = root.join("key");
    config.screen.pairing_token_file = root.join("screen-token");
    config.agent.daily_request_cap = cap;
    config.codex.model = Some("gpt-5.4".into());
    config.audit.log_dir = root.join("audit");
    config.mcp.server = vec![echo_config::McpServer {
        name: "spoof".into(),
        url: spoof.url().into(),
        auth: echo_config::McpAuth::None,
        bearer_file: None,
        allow_tools: ["test.echo", "test.wait", "test.requires_approval", "ui.show_emotion"]
            .map(String::from)
            .to_vec(),
    }];
    config
}
fn rows(root: &Path, event: &str) -> Vec<Value> {
    std::fs::read_to_string(root.join("trace"))
        .unwrap_or_default()
        .lines()
        .map(|row| serde_json::from_str::<Value>(row).unwrap())
        .filter(|row| row["event"] == event)
        .collect()
}
async fn receipt(root: &Path, event: &str) -> Value {
    loop {
        if let Some(row) = rows(root, event).last() {
            return row.clone();
        }
        tokio::task::yield_now().await;
    }
}
async fn stopped(observer: &Observer) {
    loop {
        if observer.commands().iter().any(|c| matches!(c, LinkCommand::Cut(_))) {
            observer.finish(Ok(()));
            return;
        }
        tokio::task::yield_now().await;
    }
}
fn released(config: &echo_config::BridgeConfig) {
    drop(
        echo_codex::codex_supervisor::Supervisor::new(echo_codex::codex_supervisor::SupervisorConfig::new(
            config.codex.binary.clone().unwrap(),
            config.codex.home.clone().unwrap(),
        ))
        .unwrap(),
    );
}

#[tokio::test]
async fn t3_pinned_configured_daily_cap_interrupts_matching_turn_and_resumes_next_day() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(journey()))
        .await
        .unwrap();
}
async fn journey() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();

    let spoof = echo_spoof_mcp::Spoof::start(echo_spoof_mcp::Options::default())
        .await
        .unwrap();
    let config = configure(root, &spoof, 1);
    let mut provider = Provider::start().await;
    let day = Arc::new(AtomicU64::new(7));
    let clock = day.clone();
    let owner = Configured::new(config.clone(), root.join("trace"), ([127, 0, 0, 1], 0).into())
        .unwrap()
        .with_routes(LocalRoutes::new(&provider.origin, &provider.origin).unwrap())
        .with_clock(Arc::new(move || clock.load(Ordering::SeqCst)));
    let observer = Observer::default();
    let (cancel, cancelled) = oneshot::channel();
    let work = owner.execute(
        FakeConnector::new(observer.clone()),
        async || {
            Media::create(PeerConfig::default())
                .await
                .map_err(|_| echo_bridge::Error::Unavailable)
        },
        vec![],
        async {
            cancelled.await.unwrap();
        },
    );
    let inspect = inspect(root, &mut provider, &observer, &day, cancel);
    let (result, ()) = Box::pin(async { tokio::join!(work, inspect) }).await;
    assert_eq!(result, Ok(()));
    assert_eq!(observer.commands().last(), Some(&LinkCommand::Close));
    assert_eq!(rows(root, "tools/call").len(), 2);
    assert_eq!(
        spoof
            .snapshot()
            .unwrap()
            .calls
            .iter()
            .map(|c| (c.name.as_str(), c.count))
            .collect::<Vec<_>>(),
        [("test.echo", 1), ("test.echo", 2)]
    );
    released(&config);
    provider.close().await;
    spoof.shutdown().await.unwrap();
}
async fn inspect(
    root: &Path,
    provider: &mut Provider,
    observer: &Observer,
    day: &AtomicU64,
    cancel: oneshot::Sender<()>,
) {
    receipt(root, "bridge.ready").await;
    observer.wake("daily cap", vec![]);
    let offer = provider.read().await;
    assert_eq!(offer["offer"], "fixture-offer");
    provider.send(json!({"answer":"fixture-answer"})).await;

    success(provider, "admitted").await;
    interrupted(root, provider).await;
    day.store(8, Ordering::SeqCst);
    let snapshot = success(provider, "next-day").await;
    assert_eq!(snapshot["calls"].as_array().unwrap().len(), 1);
    assert_eq!(snapshot["sidebands"], 1);
    assert_eq!(snapshot["external"], 0);
    cancel.send(()).unwrap();
    stopped(observer).await;
}

fn session_tools() -> [Value; 2] {
    [
        json!({"name":"extend_session",
            "description":"Extend listening for this voice session within its configured bound.",
            "inputSchema":{"type":"object","properties":{"seconds":{"type":"integer","minimum":1}},
                "required":["seconds"],"additionalProperties":false}}),
        json!({"name":"end_session",
            "description":"End this voice session after its current reply has played.",
            "inputSchema":{"type":"object","properties":{},"additionalProperties":false}}),
    ]
}

async fn success(provider: &mut Provider, identity: &str) -> Value {
    provider.cycle(identity, false).await;
    assert_eq!(provider.read().await, json!({"append":identity}));
    let snapshot = provider.snapshot().await;
    assert_eq!(
        snapshot["models"],
        json!([
            {"tools":["test_echo","test_wait","test_requires_approval","ui_show_emotion","extend_session","end_session"],"outputs":0},
            {"tools":["test_echo","test_wait","test_requires_approval","ui_show_emotion","extend_session","end_session"],"outputs":1}
        ])
    );
    assert_eq!(snapshot["appends"], json!([identity]));
    snapshot
}

async fn interrupted(root: &Path, provider: &mut Provider) {
    provider.cycle("refused", true).await;
    let interrupted = receipt(root, "turn.interrupted").await;
    let snapshot = provider.snapshot().await;
    // Do not turn a scheduling sample into a universal zero-work promise.
    // The separate gated rust-v0.160.0 component asserts exactly one request
    // before polling. Here every native ingress receipt must match the exact
    // final request list, with no returned tool output or post-interrupt work.
    let expected = vec![
        json!({"tools":["test_echo","test_wait","test_requires_approval","ui_show_emotion","extend_session","end_session"],"outputs":0});
        provider.model_ingress
    ];
    assert_eq!(snapshot["models"], json!(expected));
    assert_eq!(provider.snapshot().await, snapshot);
    assert_eq!(snapshot["appends"], json!([]));
    assert_eq!(snapshot["delegations"], json!(["refused"]));
    assert_eq!(snapshot["external"], 0);
    assert_eq!(rows(root, "tools/call").len(), 1);
    correlated(root, &interrupted);
}

fn correlated(root: &Path, completed: &Value) {
    for event in ["turn/interrupt", "turn.interrupt_acknowledged", "turn.interrupted"] {
        let receipts = rows(root, event);
        assert_eq!(receipts.len(), 1, "{event}");
        assert_eq!(receipts[0]["thread_id"], completed["thread_id"]);
        assert_eq!(receipts[0]["turn_id"], completed["turn_id"]);
        assert_eq!(receipts[0]["run_id"], "run-1");
    }
    let events: Vec<_> = std::fs::read_to_string(root.join("trace"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| {
            row["event"].as_str().is_some_and(|event| {
                matches!(
                    event,
                    "turn/interrupt" | "turn.interrupt_acknowledged" | "turn.interrupted"
                )
            })
        })
        .map(|row| row["event"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        events,
        ["turn/interrupt", "turn.interrupt_acknowledged", "turn.interrupted"]
    );
    assert_eq!(rows(root, "tools/call")[0]["thread_id"], completed["thread_id"]);
    assert_ne!(rows(root, "tools/call")[0]["turn_id"], completed["turn_id"]);
}

#[tokio::test]
async fn t3_pinned_configured_exhausted_call_has_zero_model_requests() {
    tokio::time::timeout(Duration::from_secs(20), Box::pin(refused()))
        .await
        .unwrap();
}
async fn refused() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();

    let spoof = echo_spoof_mcp::Spoof::start(echo_spoof_mcp::Options::default())
        .await
        .unwrap();
    let config = configure(root, &spoof, 0);
    let mut provider = Provider::start().await;
    let owner = Configured::new(config.clone(), root.join("trace"), ([127, 0, 0, 1], 0).into())
        .unwrap()
        .with_routes(LocalRoutes::new(&provider.origin, &provider.origin).unwrap());
    let observer = Observer::default();
    let work = owner.execute(
        FakeConnector::new(observer.clone()),
        async || -> Result<Media, echo_bridge::Error> { unreachable!("over-cap call opened media") },
        vec![],
        std::future::pending(),
    );
    let inspect = async {
        receipt(root, "bridge.ready").await;
        observer.wake("refused", vec![]);
    };
    let (result, ()) = Box::pin(async { tokio::join!(work, inspect) }).await;
    assert_eq!(result, Err(echo_bridge::Error::Unavailable));
    assert_eq!(
        provider.snapshot().await,
        json!({"calls":[],"sidebands":0,"external":0})
    );
    assert_eq!(spoof.snapshot().unwrap(), echo_spoof_mcp::Snapshot::default());
    assert_eq!(rows(root, "run.started"), Vec::<Value>::new());
    released(&config);
    provider.close().await;
    spoof.shutdown().await.unwrap();
}
