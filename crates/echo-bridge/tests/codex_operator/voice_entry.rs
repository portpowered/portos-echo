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
        let mut process = Process::spawn(&mut command(&fixture));
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
    let fixture = Fixture::new("chatgpt");
    std::fs::write(fixture.home.join("mode"), "signed-out").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    configure(&fixture, listener.local_addr().unwrap().port());
    let auth = b"synthetic unchanged auth bytes\n";
    std::fs::write(fixture.home.join("auth.json"), auth).unwrap();
    let mut process = Process::spawn(&mut command(&fixture));
    let _control = fixture.control().await;
    let mut screen = bridge_wire_screen::stream(&fixture.screen.to_string()).await;
    loop {
        let state = bridge_wire_screen::state(&mut screen).await;
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
    assert!(process.child.try_wait().unwrap().is_none());
    assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(listener.poll_accept(cx).is_pending())).await);
    drop(process.input.take());
    process.collect(1).await;
    assert_eq!(process.stderr().await, "echo-bridge: authorization\n");
    assert!(process.lines.next_line().await.unwrap().is_none());
    assert_eq!(std::fs::read(fixture.home.join("auth.json")).unwrap(), auth);
    super::host::released(&fixture).await;
    assert!(tokio::net::TcpStream::connect(fixture.screen).await.is_err());
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge and local Python"]
async fn codex_voice_entry_binary_unmigrated_tools_and_sim_never_fall_back_to_legacy() {
    for kind in ["tools", "loopback", "clock"] {
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
        if kind == "clock" {
            command.arg("--sim-clock");
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
