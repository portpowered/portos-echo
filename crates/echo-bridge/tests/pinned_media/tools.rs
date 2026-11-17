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

async fn exercise(negative: bool) {
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
    let initialize = json!({"tools":catalog.tools(), "negative":negative});
    input.write_all(format!("{initialize}\n").as_bytes()).await.unwrap();
    let (commands, receive) = mpsc::channel(2);
    let ((), (mut remote, echoed)) = tokio::join!(
        client(&config, &routes, (host, catalog), commands, negative),
        super::remote(&mut input, &mut output, receive)
    );
    remote.close();
    assert!((4..=8).contains(&echoed), "remote echoed {echoed} packets");
    snapshot(&mut input, &mut output).await;
    check_mcp(&spoof);
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
    negative: bool,
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
                    model: Some("gpt-5.4"),
                },
                catalog.tools().to_vec(),
                Instant::now() + Duration::from_secs(10),
            )
            .await?;
            audio_round_trip(&mut call).await;
            commands.send(true).await.unwrap();
            dispatch(&mut call, host, catalog, negative).await;
            commands.send(false).await.unwrap();
            commands.closed().await;
            call.close(Instant::now() + Duration::from_secs(3)).await?;
            Ok(())
        },
    )
    .await;
    assert_eq!(result, Ok(()));
}

async fn dispatch<R: tokio::io::AsyncRead + Unpin, W: tokio::io::AsyncWrite + Unpin>(
    call: &mut MediaCall<'_, R, W>,
    host: McpClient<NoToken>,
    catalog: Catalog,
    negative: bool,
) {
    let config = AgentConfig {
        max_tool_calls: 8,
        task_deadline_seconds: 10,
        daily_request_cap: 1,
        model: None,
    };
    let mut session = ToolSession::new(call, host, catalog, config, ToolBudget::new(1, || Ok(7))).unwrap();
    let expected = if negative {
        "I couldn't call test echo."
    } else {
        "test echo: delegated echo"
    };
    let completed = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            match session.next().await.unwrap() {
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
    assert!(completed.is_ok(), "pinned delegation: missing verified native append");
}

async fn snapshot(
    input: &mut tokio::process::ChildStdin,
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
) {
    input.write_all(b"{\"snapshot\":true}\n").await.unwrap();
    let value: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(
        value,
        json!({"calls":[{
            "path":"/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas",
            "model":"gpt-live-1-codex", "alpha":"quicksilver=v2"}],
            "sidebands":1,"external":0, "delegations":["delegation-local"],
            "models":[{"tools":["tool_0","tool_1","tool_2","tool_3"],"outputs":0},
                      {"tools":["tool_0","tool_1","tool_2","tool_3"],"outputs":1}],
            "appends":["delegation-local"]})
    );
}

fn check_mcp(spoof: &Spoof) {
    let observed = spoof.snapshot().unwrap();
    assert_eq!(observed.requests.iter().filter(|r| r.method == "tools/list").count(), 1);
    assert_eq!(observed.requests.iter().filter(|r| r.method == "tools/call").count(), 1);
    assert_eq!(observed.calls.len(), 1);
    assert_eq!(observed.calls[0].name, "test.echo");
    assert_eq!(observed.calls[0].args, json!({"text":"delegated echo"}));
    assert_eq!(observed.calls[0].count, 1);
}
