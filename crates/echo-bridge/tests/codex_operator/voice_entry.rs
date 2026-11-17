//! T3: prebuilt executable selects Codex before loopback Wyoming readiness.
use super::*;
use tokio::net::TcpListener;

fn configure(fixture: &Fixture, port: u16) {
    let path = fixture.directory.path().join("bridge.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    let start = text.find("[[mcp.server]]").unwrap();
    let end = text[start..].find("[screen]").unwrap() + start;
    let text = format!("{}[mcp]\nserver = []\n\n{}", &text[..start], &text[end..])
        .replace("wyoming_port = 16700", &format!("wyoming_port = {port}"));
    std::fs::write(path, text).unwrap();
}

fn command(fixture: &Fixture) -> Command {
    bridge_wire_auth::command(
        fixture.directory.path(),
        &[
            "--config",
            "bridge.toml",
            "--trace",
            "trace",
            "--health-listen",
            "127.0.0.1:0",
        ],
    )
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge and local Python"]
async fn codex_voice_entry_binary_authenticates_before_ready_and_reaps_after_eof() {
    for mode in ["chatgpt", "api_key"] {
        let fixture = Fixture::new(mode);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        configure(&fixture, listener.local_addr().unwrap().port());
        if mode == "api_key" {
            std::fs::write(fixture.directory.path().join("credential"), "synthetic-nominated-key\n").unwrap();
        } else {
            std::fs::create_dir(fixture.directory.path().join("credential")).unwrap();
        }
        let mut process = Process::spawn(
            command(&fixture)
                .env("HTTP_PROXY", "http://127.0.0.1:1")
                .env("CODEX_REFRESH_TOKEN_URL_OVERRIDE", "http://127.0.0.1:1/token"),
        );
        let mut satellite = tokio::time::timeout(OBSERVATION_TIMEOUT, listener.accept())
            .await
            .unwrap()
            .unwrap()
            .0;
        assert_eq!(
            std::fs::read_to_string(fixture.home.join("last-method")).unwrap(),
            "account/read"
        );
        assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "run-satellite");
        assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "describe");
        bridge_short::send(&mut satellite, "info", json!({"satellite":{"name":"synthetic"}})).await;
        assert_eq!(process.line().await, r#"{"event":"bridge.ready"}"#);
        assert_eq!(routing(&fixture), json!({"config":{}, "proxy":{}}));
        assert_eq!(
            std::fs::read_to_string(fixture.home.join("launches"))
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            ["started"]
        );
        drop(process.input.take());
        process.collect(0).await;
        assert_eq!(process.stderr().await, "");
        assert!(process.lines.next_line().await.unwrap().is_none());
        let rows = trace(&fixture.directory.path().join("trace"));
        assert_eq!(rows.iter().filter(|row| row["event"] == "bridge.ready").count(), 1);
        assert_eq!(rows.iter().filter(|row| row["event"] == "live.connected").count(), 0);
        super::host::released(&fixture).await;
        assert!(tokio::net::TcpStream::connect(fixture.screen).await.is_err());
    }
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge and local Python"]
async fn codex_voice_entry_binary_missing_configuration_never_dials_wyoming() {
    for missing in ["binary", "home"] {
        let fixture = Fixture::new("chatgpt");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        configure(&fixture, listener.local_addr().unwrap().port());
        let path = fixture.directory.path().join("bridge.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        let text = if missing == "binary" {
            text.replace(
                &fixture.binary.display().to_string(),
                &fixture.directory.path().join("missing-binary").display().to_string(),
            )
        } else {
            text.lines()
                .filter(|line| !line.starts_with("home ="))
                .collect::<Vec<_>>()
                .join("\n")
        };
        std::fs::write(path, text).unwrap();
        let mut process = Process::spawn(&mut command(&fixture));
        process.collect(2).await;
        assert_eq!(
            process.stderr().await,
            if missing == "binary" {
                "echo-bridge: codex.binary: unavailable\n"
            } else {
                "echo-bridge: codex.home: required for Codex runtime\n"
            }
        );
        assert!(process.lines.next_line().await.unwrap().is_none());
        assert!(!fixture.home.join("launches").exists());
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(listener.poll_accept(cx).is_pending())).await);
        assert!(tokio::net::TcpStream::connect(fixture.screen).await.is_err());
        assert_eq!(
            trace_if_present(&fixture.directory.path().join("trace"))
                .iter()
                .filter(|row| row["event"] == "bridge.ready")
                .count(),
            0
        );
    }
}

fn trace_if_present(path: &Path) -> Vec<Value> {
    if path.exists() { trace(path) } else { vec![] }
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge and local Python"]
async fn codex_voice_entry_binary_signed_out_shows_relink_without_device_admission() {
    rejected_account("signed-out").await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge and local Python"]
async fn codex_voice_entry_binary_invalid_account_relinks_and_recovers_on_explicit_restart() {
    rejected_account("rejected-account").await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge and local Python"]
async fn codex_voice_entry_binary_expired_account_relinks_and_recovers_on_explicit_restart() {
    rejected_account("expired-account").await;
}

async fn rejected_account(mode: &str) {
    let fixture = Fixture::new("chatgpt");
    std::fs::write(fixture.home.join("mode"), mode).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    configure(&fixture, listener.local_addr().unwrap().port());
    let auth = preserved_auth(&fixture);
    let mut process = Process::spawn(&mut command(&fixture));
    let _control = fixture.control().await;
    let mut screen = bridge_wire_screen::stream(&fixture.screen.to_string()).await;
    relink(&mut screen).await;
    denied(&fixture, &listener, &mut process).await;
    completed(&mut process, 1, "echo-bridge: authorization\n").await;
    assert_eq!(std::fs::read(fixture.home.join("auth.json")).unwrap(), auth);
    super::host::released(&fixture).await;
    assert!(tokio::net::TcpStream::connect(fixture.screen).await.is_err());
    screen_ended(&mut screen).await;
    rejected_trace(&fixture);
    recover(&fixture, &listener).await;
    assert_eq!(std::fs::read(fixture.home.join("auth.json")).unwrap(), auth);
}

async fn denied(fixture: &Fixture, listener: &TcpListener, process: &mut Process) {
    assert!(process.child.try_wait().unwrap().is_none());
    assert_no_dial(listener).await;
    // Only the initialized account boundary was reached. No implicit login,
    // thread creation, retries or legacy credential route is allowed.
    methods(fixture, &["initialize", "initialized", "account/read"]);
    super::host::released(fixture).await;
}

fn preserved_auth(fixture: &Fixture) -> &'static [u8] {
    let auth = b"synthetic unchanged auth bytes\n";
    std::fs::write(fixture.home.join("auth.json"), auth).unwrap();
    auth
}

fn rejected_trace(fixture: &Fixture) {
    let rows = trace(&fixture.directory.path().join("trace"));
    for event in [
        "bridge.ready",
        "live.connected",
        "live.created",
        "mic.completed",
        "reply.played",
    ] {
        assert_eq!(rows.iter().filter(|row| row["event"] == event).count(), 0);
    }
    let serialized = serde_json::to_string(&rows).unwrap();
    for private in [
        "synthetic private error",
        "synthetic-provider-private-detail",
        "invalid_grant",
        "invalid_token",
    ] {
        assert!(!serialized.contains(private));
    }
    assert_eq!(
        rows.iter().filter(|row| row["event"] == "auth.relink_required").count(),
        1
    );
}

async fn screen_ended(screen: &mut Lines<BufReader<tokio::net::TcpStream>>) {
    tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        // HTTP chunk/SSE separators may remain after the last parsed frame.
        while let Some(line) = screen.next_line().await.unwrap() {
            assert!(!line.starts_with("data: "), "unexpected screen frame: {line}");
        }
    })
    .await
    .unwrap();
}

async fn relink(screen: &mut Lines<BufReader<tokio::net::TcpStream>>) {
    loop {
        let state = bridge_wire_screen::state(screen).await;
        if state["state"] == "error" {
            assert_eq!(
                state,
                json!({"state":"error", "relink":true,
                "message":"Run echo-bridge signin with your config, then restart the bridge."})
            );
            break;
        }
        assert_eq!(state["state"], "ready");
    }
}

async fn assert_no_dial(listener: &TcpListener) {
    assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(listener.poll_accept(cx).is_pending())).await);
}

async fn completed(process: &mut Process, code: i32, stderr: &str) {
    drop(process.input.take());
    process.collect(code).await;
    assert_eq!(process.stderr().await, stderr);
    assert!(process.lines.next_line().await.unwrap().is_none());
}

fn methods(fixture: &Fixture, expected: &[&str]) {
    let text = std::fs::read_to_string(fixture.home.join("methods")).unwrap();
    assert_eq!(text.lines().collect::<Vec<_>>(), expected);
}

async fn recover(fixture: &Fixture, listener: &TcpListener) {
    std::fs::write(fixture.home.join("mode"), "chatgpt").unwrap();
    let mut process = Process::spawn(&mut command(fixture));
    let _satellite = admitted(&mut process, listener).await;
    let mut screen = bridge_wire_screen::stream(&fixture.screen.to_string()).await;
    assert_eq!(
        bridge_wire_screen::state(&mut screen).await,
        json!({"state":"ready", "message":"Echo Live ready"})
    );
    completed(&mut process, 0, "").await;
    recovered_process(fixture);
    super::host::released(fixture).await;
    assert!(tokio::net::TcpStream::connect(fixture.screen).await.is_err());
    screen_ended(&mut screen).await;
}

async fn admitted(process: &mut Process, listener: &TcpListener) -> tokio::net::TcpStream {
    let mut satellite = tokio::time::timeout(OBSERVATION_TIMEOUT, listener.accept())
        .await
        .unwrap()
        .unwrap()
        .0;
    assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "run-satellite");
    assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "describe");
    bridge_short::send(&mut satellite, "info", json!({"satellite":{"name":"synthetic"}})).await;
    assert_eq!(process.line().await, r#"{"event":"bridge.ready"}"#);
    satellite
}

fn recovered_process(fixture: &Fixture) {
    methods(
        fixture,
        &[
            "initialize",
            "initialized",
            "account/read",
            "initialize",
            "initialized",
            "account/read",
        ],
    );
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("launches"))
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["started", "started"]
    );
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge and local Python"]
async fn codex_voice_entry_binary_unmigrated_tools_and_epoch_never_fall_back_to_legacy() {
    for kind in ["tools", "epoch"] {
        let fixture = Fixture::new("chatgpt");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        if kind != "tools" {
            configure(&fixture, listener.local_addr().unwrap().port());
            let path = fixture.directory.path().join("bridge.toml");
            let text = std::fs::read_to_string(&path).unwrap();
            std::fs::write(
                path,
                format!("{text}\n[chatgpt_auth]\ntoken_endpoint='http://127.0.0.1:1/token'\n"),
            )
            .unwrap();
        }
        let mut command = command(&fixture);
        if kind != "tools" {
            command.args([
                "--live-loopback",
                "http://127.0.0.1:1",
                "--model-loopback",
                "http://127.0.0.1:1",
            ]);
        }
        if kind == "epoch" {
            command.args(["--sim-clock", "--sim-epoch", "1800000000"]);
        }
        let mut process = Process::spawn(&mut command);
        process.collect(2).await;
        assert_eq!(process.stderr().await, "echo-bridge: invalid input\n");
        assert!(process.lines.next_line().await.unwrap().is_none());
        assert!(!fixture.directory.path().join("trace").exists());
        assert!(!fixture.home.join("launches").exists());
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(listener.poll_accept(cx).is_pending())).await);
    }
}

fn routing(fixture: &Fixture) -> Value {
    serde_json::from_slice(&std::fs::read(fixture.home.join("routing.json")).unwrap()).unwrap()
}

fn assert_local_routes(observed: &Value, live: &str, model: &str) {
    let mut expected = json!({
        "model_provider":"local",
        "model_providers.local":{"name":"local", "base_url":format!("{model}/v1"),
            "wire_api":"responses", "requires_openai_auth":true,
            "experimental_bearer_token":"dummy-local-only"},
        "experimental_realtime_webrtc_call_base_url":format!("{live}/backend-api/codex"),
        "experimental_realtime_ws_base_url":format!("{live}/v1"),
        "features.skip_host_skill_discovery":true, "web_search":"disabled",
        "mcp_servers":{}, "cli_auth_credentials_store":"file"
    });
    for key in [
        "features.plugins",
        "features.recommended_plugins",
        "features.remote_models",
        "features.apps",
        "features.remote_control",
        "features.skill_search",
        "cloud.skills.enabled",
        "analytics.enabled",
        "feedback.enabled",
        "features.multi_agent",
        "features.multi_agent_v2",
        "features.goals",
        "tools.experimental_request_user_input.enabled",
        "features.shell_tool",
        "features.unified_exec",
        "tools.update_plan.enabled",
        "check_for_update_on_startup",
    ] {
        expected[key] = json!(false);
    }
    assert_eq!(observed["config"], expected);
    let mut proxy = json!({"NO_PROXY":"127.0.0.1", "no_proxy":"127.0.0.1",
        "CODEX_REFRESH_TOKEN_URL_OVERRIDE":format!("{live}/token")});
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        proxy[key] = json!(live);
    }
    assert_eq!(observed["proxy"], proxy);
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge and local Python"]
async fn codex_voice_entry_binary_local_routes_reach_only_owned_child_before_ready() {
    let inherited = std::env::var_os("HTTP_PROXY");
    for mode in ["chatgpt", "api_key"] {
        let fixture = Fixture::new(mode);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let live_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let model_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let live_port = live_listener.local_addr().unwrap().port();
        let model_port = model_listener.local_addr().unwrap().port();
        let live = format!("http://127.0.0.1:{live_port}"); // lint-rules: allow-literal allocated loopback fake origin
        let model = format!("http://127.0.0.1:{model_port}"); // lint-rules: allow-literal allocated loopback fake origin
        configure_routes(
            &fixture,
            listener.local_addr().unwrap().port(),
            &live,
            mode == "api_key",
        );
        let mut command = command(&fixture);
        command.args(["--live-loopback", &live, "--model-loopback", &model]);
        if mode == "api_key" {
            command.arg("--sim-clock");
        }
        command
            .env("HTTP_PROXY", "http://127.0.0.1:1")
            .env("OPENAI_API_KEY", "synthetic-inherited-key");
        let mut process = Process::spawn(&mut command);
        let mut satellite = tokio::time::timeout(OBSERVATION_TIMEOUT, listener.accept())
            .await
            .unwrap()
            .unwrap()
            .0;
        assert_local_routes(&routing(&fixture), &live, &model);
        assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "run-satellite");
        assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "describe");
        bridge_short::send(&mut satellite, "info", json!({"satellite":{"name":"synthetic"}})).await;
        assert_eq!(process.line().await, r#"{"event":"bridge.ready"}"#);
        if mode == "api_key" {
            advance_clock(&mut process).await;
        }
        drop(process.input.take());
        process.collect(0).await;
        assert_eq!(process.stderr().await, "");
        assert!(process.lines.next_line().await.unwrap().is_none());
        assert_eq!(
            std::fs::read_to_string(fixture.home.join("launches"))
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            ["started"]
        );
        super::host::released(&fixture).await;
        assert!(tokio::net::TcpStream::connect(fixture.screen).await.is_err());
    }
    assert_eq!(std::env::var_os("HTTP_PROXY"), inherited);
}

fn configure_routes(fixture: &Fixture, port: u16, live: &str, api_key: bool) {
    configure(fixture, port);
    let path = fixture.directory.path().join("bridge.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, format!("{text}\n[chatgpt_auth]\ntoken_endpoint='{live}/token'\n")).unwrap();
    if api_key {
        std::fs::write(fixture.directory.path().join("credential"), "synthetic-nominated-key\n").unwrap();
    } else {
        std::fs::create_dir(fixture.directory.path().join("credential")).unwrap();
    }
}

async fn advance_clock(process: &mut Process) {
    process
        .input
        .as_mut()
        .unwrap()
        .write_all(b"{\"action\":\"advance_clock\",\"milliseconds\":1000}\n")
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&process.line().await).unwrap(),
        json!({"event":"bridge.clock_advanced", "milliseconds":1000})
    );
}
