//! Configured T3 journey through the pinned app server and fake Wyoming/Responses/Live.
use super::*;
use std::{fs, future::Future, os::unix::fs::PermissionsExt};
use tokio::io::Lines;

pub(super) type Output = Lines<BufReader<tokio::process::ChildStdout>>;
const MODEL: &str = "gpt-6.1-sol";
const SKILLS: &str =
    "\n\nEnabled owner skills (use read_skill by name to load the body):\n- brief — Keep the reply brief.";
const BODY: &str = "Summarize the answer in one sentence.\n";

#[tokio::test]
async fn t3_pinned_runner_skills_reads_selected_body_and_refuses_disabled_and_unknown() {
    tokio::time::timeout(
        Duration::from_secs(25),
        Box::pin(configured_journey(Some("['brief']"), true, None)),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn t3_pinned_runner_skills_empty_and_omitted_keep_the_default_thread() {
    tokio::time::timeout(Duration::from_secs(25), async {
        tokio::join!(
            Box::pin(configured_journey(None, false, None)),
            Box::pin(configured_journey(Some("[]"), false, None))
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn t3_pinned_runner_skills_disabled_preserves_mcp_name_and_alias() {
    tokio::time::timeout(Duration::from_secs(25), async {
        tokio::join!(
            Box::pin(configured_journey(None, false, Some("read.skill"))),
            Box::pin(configured_journey(Some("[]"), false, Some("read_skill")))
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn t3_pinned_runner_skills_unknown_selection_exits_before_runtime() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let wyoming = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
    configure_skills(root, wyoming.local_addr().unwrap().port(), Some("['unlisted']"));
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        launch(root, &loopback_origin(provider.local_addr().unwrap().port())).wait_with_output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(output.stdout, b"");
    assert_eq!(output.stderr, b"echo-bridge: codex.skills[0]: unknown skill\n");
    for name in ["skills-rpc.jsonl", "trace", "home"] {
        assert!(!root.join(name).exists(), "startup created {name}");
    }
    for listener in [wyoming, provider] {
        assert_eq!(
            listener.into_std().unwrap().accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
    writeln!(
        std::io::stderr(),
        "runner skills invalid selection: exit 2, no runtime effects"
    )
    .unwrap();
}

async fn configured_journey(selection: Option<&str>, enabled: bool, mcp: Option<&str>) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = configure_skills(root, listener.local_addr().unwrap().port(), selection);
    let (mut provider, mut control, mut observed) = start_provider();
    let ready = read_json(&mut observed).await;
    let origin = ready["origin"].as_str().unwrap();
    let tools = declarations(enabled, mcp);
    if let Some(name) = mcp {
        configure_mcp(root, origin, name);
    }
    if enabled || mcp.is_some() {
        send_json(&mut control, json!({"runner_skills":tools,"skills_mcp":mcp})).await;
        assert_eq!(read_json(&mut observed).await, json!({"initialized":"runner-skills"}));
    }
    let mut bridge = launch(root, origin);
    let (mut satellite, stdin, mut peer, stdout) = Box::pin(admit_journey(
        root,
        &listener,
        &config,
        &mut bridge,
        &mut control,
        &mut observed,
    ))
    .await;
    observe_journey(root, &tools, enabled, mcp, &mut peer, &mut control, &mut observed).await;
    drop(stdin);
    acknowledge_cut(&mut satellite).await;
    let output = bridge.wait_with_output().await.unwrap();
    drop(stdout);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    peer.close();
    drop(control);
    assert!(provider.wait().await.unwrap().success());
}

/// Admits the bridge and connects its call. The bridge's stdout comes back
/// open: a closed pipe fails its next `bridge.*` line.
pub(super) async fn admit_journey(
    root: &Path,
    listener: &TcpListener,
    config: &CodexConfig,
    bridge: &mut tokio::process::Child,
    control: &mut tokio::process::ChildStdin,
    observed: &mut Output,
) -> (TcpStream, tokio::process::ChildStdin, LivePeer, Output) {
    let mut stdin = bridge.stdin.take().unwrap();
    let (mut satellite, _) = listener.accept().await.unwrap();
    let mut stdout = BufReader::new(bridge.stdout.take().unwrap()).lines();
    admit(&mut satellite, &mut stdout, config).await;
    let mut screen = lifecycle::screen(root).await;
    wake(&mut satellite).await;
    let mut peer = connect_provider(control, observed).await;
    let connected =
        async { while lifecycle::next_frame(&mut screen).await.unwrap()["state"]["state"] != "connected" {} };
    await_connected(&mut peer, &mut stdin, &mut stdout, connected).await;
    (satellite, stdin, peer, stdout)
}

async fn observe_journey(
    root: &Path,
    tools: &[Value],
    enabled: bool,
    mcp: Option<&str>,
    peer: &mut LivePeer,
    control: &mut tokio::process::ChildStdin,
    observed: &mut Output,
) {
    if enabled || mcp.is_some() {
        send_json(control, json!({"transcripts":true})).await;
        assert_eq!(
            drive_peer(peer, read_json(observed)).await,
            json!({"append":"runner-skills"})
        );
    }
    send_json(control, json!({"session":true})).await;
    let context = voice_context(&read_json(observed).await);
    let rows = read_rpc(root);
    check_thread(&rows[0], &context, tools, enabled);
    if let Some(name) = mcp {
        send_json(control, json!({"snapshot":"runner-skills"})).await;
        check_mcp_results(&rows[1..], &read_json(observed).await, name);
    } else if enabled {
        send_json(control, json!({"snapshot":"runner-skills"})).await;
        check_skill_results(&rows[1..], &read_json(observed).await);
    } else {
        assert_eq!(rows.len(), 1);
    }
}

pub(super) fn start_provider() -> (tokio::process::Child, tokio::process::ChildStdin, Output) {
    let mut provider = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pinned_media/provider.py"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let control = provider.stdin.take().unwrap();
    let observed = BufReader::new(provider.stdout.take().unwrap()).lines();
    (provider, control, observed)
}

pub(super) async fn acknowledge_cut(satellite: &mut TcpStream) {
    loop {
        let (header, payload) = read_wire(satellite).await;
        assert_eq!(payload, b"");
        match header["type"].as_str().unwrap() {
            "audio-start" => (),
            "hassmic-thinking" => {
                let thinking = header["data"]["thinking"].as_bool().unwrap();
                assert_eq!(header["data"], json!({"thinking":thinking}));
            }
            kind => {
                assert_eq!(kind, "hassmic-flush");
                send(satellite, "played", json!({}), vec![]).await;
                return;
            }
        }
    }
}

pub(super) async fn drive_peer<T>(peer: &mut LivePeer, work: impl Future<Output = T>) -> T {
    tokio::pin!(work);
    loop {
        tokio::select! {
            () = step(peer) => (),
            value = &mut work => return value,
        }
    }
}

async fn await_connected(
    peer: &mut LivePeer,
    stdin: &mut tokio::process::ChildStdin,
    stdout: &mut Output,
    connected: impl Future<Output = ()>,
) {
    tokio::select! {
        () = drive_peer(peer, connected) => (),
        () = tick_clock(stdin, stdout) => (),
    }
}

// The executable uses paused time. Run its protocol timers while ICE and
// DTLS negotiate, and use the Connected frame as the readiness barrier.
async fn tick_clock(stdin: &mut tokio::process::ChildStdin, stdout: &mut Output) {
    let mut ticks = tokio::time::interval(Duration::from_millis(20));
    loop {
        ticks.tick().await;
        send_json(stdin, json!({"action":"advance_clock","milliseconds":20})).await;
        assert_eq!(
            read_json(stdout).await,
            json!({"event":"bridge.clock_advanced","milliseconds":20})
        );
    }
}

pub(super) fn configure_skills(root: &Path, port: u16, selection: Option<&str>) -> CodexConfig {
    let config = configure(root, port);
    let wrapper = root.join("skills-observer");
    fs::write(&wrapper, include_bytes!("runner_skills_stdio.py")).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let pin = config.binary.as_ref().unwrap().to_str().unwrap();
    fs::write(root.join("pin-path"), pin).unwrap();
    let prompts = root.join("agent-home/prompts");
    fs::create_dir_all(&prompts).unwrap();
    fs::write(prompts.join("runner.md"), "Fixture runner.").unwrap();
    fs::write(prompts.join("voice.md"), "Fixture voice.").unwrap();
    for (name, content) in [
        ("brief", include_str!("../../../../../examples/skills/brief/SKILL.md")),
        (
            "unused",
            "---\nname: unused\ndescription: Disabled description.\n---\nDisabled body.\n",
        ),
    ] {
        let skill = root.join("agent-home/skills").join(name);
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), content).unwrap();
    }
    let file = root.join("bridge.toml");
    let base = fs::read_to_string(&file)
        .unwrap()
        .replace(pin, wrapper.to_str().unwrap());
    let selection = selection.map_or_else(String::new, |value| format!("skills = {value}\n"));
    fs::write(
        file,
        format!(
            "{base}web_search = 'disabled'\ndelegate_model = '{MODEL}'\n{selection}\n[home]\ndir = '{}'\nagent_tools = false\n",
            root.join("agent-home").display()
        ),
    )
    .unwrap();
    config
}

fn loopback_origin(port: u16) -> String {
    const LOOPBACK: &str = "http://127.0.0.1";
    format!("{LOOPBACK}:{port}")
}

fn configure_mcp(root: &Path, origin: &str, name: &str) {
    let file = root.join("bridge.toml");
    let mcp = format!(
        "[[mcp.server]]\nname = 'fixture'\nurl = '{origin}/skills-mcp'\nauth = 'none'\nallow_tools = ['{name}']"
    );
    let config = fs::read_to_string(&file).unwrap().replace("[mcp]\nserver = []", &mcp);
    fs::write(file, config).unwrap();
}

async fn connect_provider(control: &mut tokio::process::ChildStdin, observed: &mut Output) -> LivePeer {
    let offer = read_json(observed).await;
    let mut peer = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now().into_std())
        .await
        .unwrap();
    let offer = offer["offer"].as_str().unwrap();
    loopback_offer(offer);
    let answer = peer.answer(offer).await.unwrap();
    send_json(control, json!({"answer":answer})).await;
    peer
}

pub(super) async fn send_json(control: &mut tokio::process::ChildStdin, value: Value) {
    control.write_all(format!("{value}\n").as_bytes()).await.unwrap();
}

pub(super) async fn read_json(output: &mut Output) -> Value {
    serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap()
}

pub(super) fn read_rpc(root: &Path) -> Vec<Value> {
    fs::read_to_string(root.join("skills-rpc.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

pub(super) fn voice_context(observed: &Value) -> String {
    let voice = observed["session"]["instructions"].as_str().unwrap();
    let expected = format!("{}\n\nFixture voice.\n", echo_config::LIVE_SESSION_GUIDANCE);
    let context = voice.strip_prefix(&expected).unwrap();
    assert!(context.starts_with("[Call-start context: "));
    assert!(context.ends_with("UTC+00:00; local offset unavailable]")); // lint-rules: allow-literal UTC calendar expectation, never an endpoint
    assert_eq!(context.lines().count(), 1);
    assert_eq!(
        observed,
        &json!({"session":{"instructions":voice,"initial_items":[{
            "type":"message","role":"developer","content":[{"type":"input_text","text":voice}]
        }]}})
    );
    writeln!(std::io::stderr(), "runner skills Live prompt: {observed}").unwrap();
    context.into()
}

pub(super) fn declarations(enabled: bool, mcp: Option<&str>) -> Vec<Value> {
    let mut tools = vec![
        json!({"name":"extend_session","description":"Extend listening for this voice session within its configured bound.",
            "inputSchema":{"type":"object","properties":{"seconds":{"type":"integer","minimum":1}},"required":["seconds"],"additionalProperties":false}}),
        json!({"name":"end_session","description":"End this voice session now, stopping any reply and work still in progress.",
            "inputSchema":{"type":"object","properties":{},"additionalProperties":false}}),
    ];
    if enabled {
        tools.push(json!({"name":"read_skill","description":"Read the Markdown body of an enabled owner skill.",
            "inputSchema":{"type":"object","properties":{"name":{"type":"string"}},"required":["name"],"additionalProperties":false}}));
    }
    if mcp.is_some() {
        tools.insert(
            0,
            json!({"name":"read_skill","description":"Fixture MCP reader.",
            "inputSchema":{"type":"object","properties":{"name":{"type":"string"}},
                "required":["name"],"additionalProperties":false}}),
        );
    }
    tools
}

pub(super) fn check_thread(observed: &Value, context: &str, tools: &[Value], enabled: bool) {
    let skills = if enabled { SKILLS } else { "" };
    let runner = format!(
        "Fixture runner.\n\n{}\n{context}{skills}",
        echo_config::RUNNER_SESSION_GUIDANCE
    );
    assert!(observed["id"].as_u64().is_some());
    assert_eq!(
        observed,
        &json!({
            "id":observed["id"],"method":"thread/start","params":{
                "ephemeral":true,"approvalPolicy":"never","sandbox":"read-only","environments":[],
                "config":{
                    "features.view_image":false,"features.shell_tool":false,"features.unified_exec":false,"features.multi_agent":false,
                    "features.multi_agent_v2":false,"agents.enabled":false,"features.goals":false,
                    "tools.experimental_request_user_input.enabled":false,"tools.update_plan.enabled":false,
                    "features.code_mode.direct_only_tool_namespaces":["functions"],"web_search":"disabled",
                    "mcp_servers":{},"skills.include_instructions":false
                },
                "model":MODEL,"serviceTier":null,"baseInstructions":runner,"dynamicTools":tools
            }
        })
    );
    writeln!(std::io::stderr(), "runner skills thread request: {observed}").unwrap();
}

fn check_mcp_results(rows: &[Value], provider: &Value, name: &str) {
    assert_eq!(provider["external"], 0);
    assert_eq!(provider["appends"], json!(["runner-skills"]));
    assert_eq!(
        provider["mcp_calls"],
        json!([{"name":name,"arguments":{"name":"brief"}}])
    );
    assert_eq!(rows.len(), 2);
    let result = json!([{"type":"text","text":"MCP delivered brief."}]).to_string();
    for (index, (id, text, success)) in [
        ("skill-brief", result.as_str(), true),
        ("skill-malformed", "tool request refused", false),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            rows[index]["result"],
            json!({"contentItems":[{"type":"inputText","text":text}],"success":success})
        );
        assert_eq!(provider["skill_outputs"][index]["call_id"], id);
        assert_eq!(provider["skill_outputs"][index]["output"], text);
    }
}

fn check_skill_results(rows: &[Value], provider: &Value) {
    assert_eq!(rows.len(), 4);
    assert_eq!(provider["external"], 0);
    assert_eq!(provider["appends"], json!(["runner-skills"]));
    let output = provider["skill_outputs"].as_array().unwrap();
    assert_eq!(output.len(), 4);
    for (index, (name, payload)) in [
        ("brief", json!({"body":BODY})),
        ("unused", json!({"error":"skill_not_enabled"})),
        ("unlisted", json!({"error":"skill_not_enabled"})),
        ("malformed", json!({"error":"invalid_arguments"})),
    ]
    .into_iter()
    .enumerate()
    {
        let text = payload.to_string();
        assert_eq!(
            rows[index]["result"],
            json!({"contentItems":[{"type":"inputText","text":text}],"success":index == 0})
        );
        assert_eq!(output[index]["call_id"], format!("skill-{name}"));
        assert_eq!(output[index]["output"], text);
    }
    writeln!(std::io::stderr(), "runner skills RPC responses: {}", json!(rows)).unwrap();
    writeln!(
        std::io::stderr(),
        "runner skills fake model results: {}",
        provider["skill_outputs"]
    )
    .unwrap();
}
