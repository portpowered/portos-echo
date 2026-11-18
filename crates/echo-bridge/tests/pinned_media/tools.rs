//! T3: actual pinned engine -> production `ToolSession` -> loopback MCP -> append.
//! Component witness only; the full M4 scenario/golden remains independently required.
use super::*;
use echo_agent_api::ToolHost;
use echo_bridge::bridge_codex::{ToolBudget, ToolSession};
use echo_config::{AgentConfig, McpAuth, McpServer};
use echo_delegation::codex_dispatch::Catalog;
use echo_mcp_client::{McpClient, TokenError, TokenProvider};
use echo_spoof_mcp::{Fault, Spoof};

struct NoToken;
impl TokenProvider for NoToken {
    fn token(&self) -> impl Future<Output = Result<Secret<String>, TokenError>> {
        std::future::ready(Ok(Secret::new(String::new())))
    }
}

#[tokio::test]
async fn t3_pinned_tools_validate_catalog_result_and_native_append() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(exercise(false)))
        .await
        .unwrap();
}

#[tokio::test]
async fn t3_pinned_tools_report_mcp_failure_without_retry_or_false_success() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(exercise(true)))
        .await
        .unwrap();
}

#[tokio::test]
async fn t3_pinned_tools_native_code_mode_executes_original_mcp() {
    tokio::time::timeout(
        Duration::from_secs(25),
        Box::pin(exercise_mode(false, false, ToolBudget::new(1, || Ok(7)), "gpt-6.1-sol")),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn t3_pinned_tools_native_code_mode_reports_mcp_failure() {
    tokio::time::timeout(
        Duration::from_secs(25),
        Box::pin(exercise_mode(true, false, ToolBudget::new(1, || Ok(7)), "gpt-6.1-sol")),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn t3_pinned_tools_daily_cap_interrupts_without_mcp() {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    tokio::time::timeout(
        Duration::from_secs(25),
        Box::pin(async {
            let day = Arc::new(AtomicU64::new(7));
            let clock = day.clone();
            let budget = ToolBudget::new(1, move || Ok(clock.load(Ordering::SeqCst)));
            budget.admit_turn().unwrap();
            assert!(budget.check_call().is_err());
            Box::pin(exercise_mode(false, true, budget.clone(), "gpt-5.4")).await;
            assert!(budget.check_call().is_err());
            day.store(8, Ordering::SeqCst);
            budget.check_call().unwrap();
            Box::pin(exercise_mode(false, false, budget.clone(), "gpt-5.4")).await;
            assert!(budget.check_call().is_err());
        }),
    )
    .await
    .unwrap();
}

async fn exercise(negative: bool) {
    Box::pin(exercise_mode(negative, false, ToolBudget::new(1, || Ok(7)), "gpt-5.4")).await;
}

async fn exercise_mode(negative: bool, exhausted: bool, budget: ToolBudget, model: &str) {
    let directory = tempfile::tempdir().unwrap();
    private_parent(directory.path());
    let config = CodexConfig {
        binary: Some(binary()),
        home: Some(directory.path().join("home")),
        ..CodexConfig::default()
    };
    let spoof = Spoof::start(echo_spoof_mcp::Options::default()).await.unwrap();
    let (host, catalog) = catalog(&spoof).await;
    if negative {
        spoof.set_fault(Fault::Http500).unwrap();
    }
    let mut provider = Command::new(if cfg!(windows) { "python" } else { "python3" })
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pinned_media/provider.py"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = provider.stdin.take().unwrap();
    let mut output = BufReader::new(provider.stdout.take().unwrap()).lines();
    let ready: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    let origin = ready["origin"].as_str().unwrap();
    let routes = LocalRoutes::new(origin, origin).unwrap();
    let initialize = json!({"tools":catalog.tools(), "negative":negative, "exhausted":exhausted, "model":model});
    input.write_all(format!("{initialize}\n").as_bytes()).await.unwrap();
    let (commands, receive) = mpsc::channel(2);
    let (started, model_started) = tokio::sync::oneshot::channel();
    let ((), (mut remote, echoed)) = tokio::join!(
        client(
            &config,
            &routes,
            (host, catalog),
            commands,
            (negative, exhausted, model_started, budget, model)
        ),
        remote(&mut input, &mut output, receive, started)
    );
    remote.close();
    assert!((4..=8).contains(&echoed), "remote echoed {echoed} packets");
    snapshot(&mut input, &mut output, exhausted, model).await;
    check_mcp(&spoof, exhausted);
    drop(input);
    assert!(provider.wait().await.unwrap().success());
    spoof.shutdown().await.unwrap();
    drop(
        echo_codex::codex_supervisor::Supervisor::new(echo_codex::codex_supervisor::SupervisorConfig::new(
            config.binary.unwrap(),
            config.home.unwrap(),
        ))
        .unwrap(),
    );
}

async fn catalog(spoof: &Spoof) -> (McpClient<NoToken>, Catalog) {
    let allowed = ["test.echo", "test.wait", "test.requires_approval", "ui.show_emotion"]
        .map(String::from)
        .to_vec();
    let host = McpClient::new(
        McpServer {
            name: "spoof".into(),
            url: spoof.url().into(),
            auth: McpAuth::None,
            bearer_file: None,
            allow_tools: allowed.clone(),
        },
        NoToken,
        Duration::from_secs(3),
    )
    .unwrap();
    let catalog = Catalog::new(host.list_tools().await.unwrap(), &allowed).unwrap();
    for (index, name) in allowed.iter().enumerate() {
        assert_eq!(catalog.resolve(&format!("tool_{index}")), Some(name.as_str()));
    }
    (host, catalog)
}

async fn client(
    config: &CodexConfig,
    routes: &LocalRoutes,
    (host, catalog): (McpClient<NoToken>, Catalog),
    commands: mpsc::Sender<bool>,
    (negative, exhausted, model_started, budget, model): (
        bool,
        bool,
        tokio::sync::oneshot::Receiver<()>,
        ToolBudget,
        &str,
    ),
) {
    let key = Secret::new("dummy-local-only".to_owned());
    let result = with_host_options(
        HostOptions {
            config,
            mode: LiveMode::ApiKey,
            api_key: Some(&key),
            routes: Some(routes),
            deadline: Instant::now() + Duration::from_secs(10),
        },
        std::future::pending(),
        async |io| {
            let peer = Peer::create(PeerConfig {
                interface_allowlist: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
                allow_loopback: true,
                queue_capacity: NonZeroUsize::new(32).unwrap(),
                ..PeerConfig::default()
            })
            .await
            .unwrap();
            let mut call = MediaCall::new(io, peer);
            call.start(
                &Options {
                    voice: None,
                    instructions: "Client audio bridge",
                    model: Some(model),
                    effort: None,
                    time_zone: None,
                },
                catalog.tools().to_vec(),
                Instant::now() + Duration::from_secs(10),
            )
            .await?;
            audio_round_trip(&mut call).await;
            commands.send(true).await.unwrap();
            if exhausted {
                model_started.await.unwrap();
            }
            dispatch(&mut call, host, catalog, (negative, exhausted), budget).await;
            commands.send(false).await.unwrap();
            commands.closed().await;
            call.close(Instant::now() + Duration::from_secs(3)).await?;
            Ok(())
        },
    )
    .await;
    assert_eq!(result, Ok(()));
}

async fn remote(
    input: &mut tokio::process::ChildStdin,
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    mut commands: mpsc::Receiver<bool>,
    started: tokio::sync::oneshot::Sender<()>,
) -> (LivePeer, usize) {
    let request: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    let mut peer = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now().into_std())
        .await
        .unwrap();
    let answer = peer.answer(request["offer"].as_str().unwrap()).await.unwrap();
    input
        .write_all(format!("{}\n", json!({"answer":answer})).as_bytes())
        .await
        .unwrap();
    let mut started = Some(started);
    let mut echoed = 0;
    loop {
        echoed += super::echo_ready(&mut peer).await;
        tokio::select! {
            () = step(&mut peer) => (),
            observation = output.next_line() => {
                let value: Value = serde_json::from_str(&observation.unwrap().unwrap()).unwrap();
                assert_eq!(value, json!({"model_started":true}));
                started.take().expect("pinned daily cap: extra model request after admission refusal").send(()).unwrap();
            },
            command = commands.recv() => match command {
                Some(true) => input.write_all(b"{\"transcripts\":true}\n").await.unwrap(),
                command => {
                    assert_eq!(command, Some(false));
                    return (peer, echoed);
                },
            },
        }
    }
}

async fn dispatch<R: tokio::io::AsyncRead + Unpin, W: tokio::io::AsyncWrite + Unpin>(
    call: &mut MediaCall<'_, R, W>,
    host: McpClient<NoToken>,
    catalog: Catalog,
    (negative, exhausted): (bool, bool),
    budget: ToolBudget,
) {
    let config = AgentConfig {
        max_tool_calls: 8,
        task_deadline_seconds: 10,
        daily_request_cap: 1,
        model: None,
    };
    let call_thread = call.thread_id().map(str::to_owned);
    let mut session = ToolSession::new(call, host, catalog, config, budget).unwrap();
    let mut started = None;
    let expected = if negative {
        "I couldn't call test echo."
    } else {
        "test echo: delegated echo"
    };
    let completed = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            match session.next().await.unwrap() {
                MediaEvent::Control(Event::TurnStarted { thread, turn }) => {
                    assert_eq!(Some(thread.as_str()), call_thread.as_deref());
                    assert!(started.replace((thread, turn)).is_none());
                }
                MediaEvent::Control(Event::TurnCompleted { thread, turn }) if exhausted => {
                    assert_eq!(started, Some((thread, turn)));
                    return;
                }
                MediaEvent::Control(Event::ReplyDone { text }) => {
                    assert_eq!(text, expected);
                    return;
                }
                event => assert!(
                    !matches!(event, MediaEvent::Control(Event::Closed)),
                    "call closed before native appended result"
                ),
            }
        }
    })
    .await;
    assert!(
        completed.is_ok(),
        "pinned delegation: missing verified native append or interrupted completion"
    );
}

async fn snapshot(
    input: &mut tokio::process::ChildStdin,
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    exhausted: bool,
    model: &str,
) {
    input.write_all(b"{\"snapshot\":true}\n").await.unwrap();
    let value: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    let names = if model == "gpt-5.4" {
        json!(["tool_0", "tool_1", "tool_2", "tool_3"])
    } else {
        json!([
            "functions.exec",
            "functions.wait",
            "functions.request_user_input_async",
            "clock.sleep"
        ])
    };
    if exhausted {
        // rust-v0.160.0: release exactly one request before bridge polling.
        // This is the pre-notification window, not a universal zero-work claim.
        assert_eq!(
            value,
            json!({"calls":[{
            "path":"/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas",
            "model":"gpt-live-1-codex", "alpha":"quicksilver=v2"}],
            "sidebands":1,"external":0,"delegations":["delegation-local"],
            "models":[{"tools":names,"outputs":0}],"appends":[]})
        );
        return;
    }
    assert_eq!(
        value,
        json!({"calls":[{
            "path":"/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas",
            "model":"gpt-live-1-codex", "alpha":"quicksilver=v2"}],
            "sidebands":1,"external":0, "delegations":["delegation-local"],
            "models":[{"tools":names,"outputs":0},
                      {"tools":names,"outputs":1}],
            "appends":["delegation-local"]})
    );
}

fn check_mcp(spoof: &Spoof, exhausted: bool) {
    let observed = spoof.snapshot().unwrap();
    assert_eq!(observed.requests.iter().filter(|r| r.method == "tools/list").count(), 1);
    if exhausted {
        assert_eq!(observed.requests.iter().filter(|r| r.method == "tools/call").count(), 0);
        assert_eq!(observed.calls.len(), 0);
        return;
    }
    assert_eq!(observed.requests.iter().filter(|r| r.method == "tools/call").count(), 1);
    assert_eq!(observed.calls.len(), 1);
    assert_eq!(observed.calls[0].name, "test.echo");
    assert_eq!(observed.calls[0].args, json!({"text":"delegated echo"}));
    assert_eq!(observed.calls[0].count, 1);
}
