//! T3: config through authenticated service; controlled process and device ports.
use super::*;
use echo_bridge::{Error, bridge_voice::Configured};
use echo_link::fake::{Command, FakeConnector, Observer};

/// Refuses the first connection, then behaves like the fake device.
#[derive(Clone, Default)]
struct RefusedDevice {
    entered: std::sync::Arc<tokio::sync::Notify>,
    release: std::sync::Arc<tokio::sync::Notify>,
    refused: std::sync::Arc<std::sync::atomic::AtomicBool>,
    observer: Observer,
}

impl echo_link::LinkConnector for RefusedDevice {
    type Link = echo_link::fake::FakeLink;

    async fn connect(&self) -> Result<Self::Link, echo_link::Error> {
        if self.refused.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return FakeConnector::new(self.observer.clone()).connect().await;
        }
        self.entered.notify_one();
        self.release.notified().await;
        Err(echo_link::Error::Transport)
    }
}

// A device that cannot be reached at startup is not a reason to exit: the
// error is shown and traced, and the bridge retries until it is ready.
#[tokio::test]
#[ignore = "T3 controlled process and paired loopback screen"]
async fn codex_configured_refused_device_shows_error_then_retries_to_ready() {
    let fixture = Fixture::new("chatgpt");
    let config = config(&fixture);
    let path = fixture.directory.path().join("trace");
    let connector = RefusedDevice::default();
    let prepared = Configured::new(config, path.clone(), ([127, 0, 0, 1], 0).into()).unwrap();
    let (stop, stopping) = tokio::sync::oneshot::channel();
    let work = prepared.execute(
        connector.clone(),
        async || -> Result<echo_media::Peer, Error> { unreachable!() },
        vec![],
        async {
            stopping.await.unwrap();
        },
    );
    let inspect = async {
        let _control = fixture.control().await;
        connector.entered.notified().await;
        let mut screen = bridge_wire_screen::stream(&fixture.screen.to_string()).await;
        connector.release.notify_one();
        let mut shown = false;
        loop {
            let state = bridge_wire_screen::state(&mut screen).await;
            if state["state"] == "error" {
                assert_eq!(
                    state,
                    json!({"state":"error", "message":"Conversation unavailable. Try another wake.", "relink":false})
                );
                shown = true;
            } else if shown && state["message"] == "Echo Live ready" {
                break;
            }
        }
        stop.send(()).unwrap();
    };
    let (result, ()) = Box::pin(tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        tokio::join!(work, inspect)
    }))
    .await
    .unwrap();
    assert_eq!(result, Ok(()));
    assert_eq!(
        connector.observer.commands(),
        [Command::Connect, Command::Capture, Command::Probe, Command::Close]
    );
    super::host::released(&fixture).await;
    presentation_released(&path, "chatgpt", true);
    let rows = trace(&path);
    let count = |event: &str| rows.iter().filter(|row| row["event"] == event).count();
    assert_eq!((count("bridge.error"), count("bridge.ready")), (0, 1));
    let retries: Vec<_> = rows.iter().filter(|row| row["event"] == "bridge.recovering").collect();
    assert_eq!(retries.len(), 1);
    assert_eq!(
        (&retries[0]["reason"], &retries[0]["attempt"], &retries[0]["delay_ms"]),
        (&json!("wyoming"), &json!(1), &json!(1000))
    );
    let launches = std::fs::read_to_string(fixture.home.join("launches")).unwrap();
    assert_eq!(launches.lines().count(), 2);
}

fn config(fixture: &Fixture) -> echo_config::BridgeConfig {
    let mut config = echo_config::BridgeConfig::load(fixture.directory.path().join("bridge.toml")).unwrap();
    config.audit.log_dir = fixture.directory.path().join("audit");
    config.live.credential_file = fixture.directory.path().join("nominated-key");
    config
}

// T3: the runtime's actual HTTP, settings store and owned controlled child.
// This observes a cleanup-complete restart request, not binary exec.
#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn t3_settings_runtime_mounts_authorized_host_and_releases_before_restart() {
    for restart in [false, true] {
        let fixture = Fixture::new("chatgpt");
        let mut base = config(&fixture);
        let home = fixture.directory.path().join("agent-home");
        base.home = Some(echo_config::HomeConfig { dir: home.clone() });
        std::fs::create_dir_all(home.join("prompts")).unwrap();
        std::fs::write(home.join("prompts/voice.md"), "admitted voice").unwrap();
        std::fs::write(home.join("prompts/runner.md"), "admitted runner").unwrap();
        base.agent.max_tool_calls = 6;
        let overlay = echo_config::SettingsOverlay::from_toml_str("[agent]\nmax_tool_calls = 8\n").unwrap();
        std::fs::write(home.join("settings.toml"), overlay.to_toml_string().unwrap()).unwrap();
        let effective = echo_config::EffectiveConfig {
            config: overlay.apply_to(&base).unwrap(),
            base,
            overlay,
            warning: None,
        };
        let observer = Observer::default();
        let path = fixture.directory.path().join("runtime-trace");
        let (stop, stopping) = tokio::sync::oneshot::channel();
        let linked = std::cell::Cell::new(false);
        let configured = Configured::from_effective(effective, path.clone(), ([127, 0, 0, 1], 0).into()).unwrap();
        let work = configured.execute_with_settings(
            FakeConnector::new(observer.clone()),
            async || -> Result<echo_media::Peer, Error> { unreachable!("no wake admits no peer") },
            vec![],
            async {
                stopping.await.unwrap();
            },
            || {
                if linked.get() {
                    echo_screen_contract::settings_v1::LinkStatusV1::Linked
                } else {
                    echo_screen_contract::settings_v1::LinkStatusV1::LinkRequired
                }
            },
        );
        let inspect = async {
            let _control = fixture.control().await;
            let mut screen = bridge_wire_screen::stream(&fixture.screen.to_string()).await;
            while bridge_wire_screen::state(&mut screen).await["message"] != "Echo Live ready" {}
            settings_runtime_requests(&fixture, &linked, &home).await;
            presentation_owned(&path);
            assert!(!observer.commands().contains(&Command::Close));
            if restart {
                let response = settings_runtime_patch(&fixture, json!({"agent":{"max_tool_calls":3}})).await;
                assert_eq!(response, json!({"version":1,"restart_pending":true}));
                assert_eq!(
                    bridge_wire_screen::state(&mut screen).await,
                    json!({"state":"ready","message":"Restarting bridge..."})
                );
                while let Some(line) = screen.next_line().await.unwrap() {
                    assert!(!line.starts_with("data:"));
                }
            } else {
                stop.send(()).unwrap();
            }
        };
        let (result, ()) = Box::pin(tokio::time::timeout(OBSERVATION_TIMEOUT, async {
            tokio::join!(work, inspect)
        }))
        .await
        .unwrap();
        assert_eq!(result, Ok(restart));
        assert_eq!(
            observer.commands(),
            [Command::Connect, Command::Capture, Command::Probe, Command::Close]
        );
        super::host::released(&fixture).await;
        presentation_released(&path, "chatgpt", true);
        let rows = trace(&path);
        assert_eq!(
            rows.iter()
                .filter(|row| row["event"] == "settings.restart_prepared")
                .count(),
            usize::from(restart)
        );
        assert!(tokio::net::TcpStream::connect(fixture.screen).await.is_err());
    }
}

async fn settings_runtime_patch(fixture: &Fixture, settings: serde_json::Value) -> serde_json::Value {
    let response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/settings/v1?token=synthetic-screen-pair",
            fixture.screen.port()
        ))
        .json(&json!({"version":1,"settings":settings}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    response.json().await.unwrap()
}

async fn settings_runtime_requests(fixture: &Fixture, linked: &std::cell::Cell<bool>, home: &Path) {
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{}/settings/v1", fixture.screen.port());
    assert_eq!(
        client.get(&url).send().await.unwrap().status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let url = format!("{url}?token=synthetic-screen-pair");
    for (value, expected) in [(false, "link_required"), (true, "linked")] {
        linked.set(value);
        let snapshot: serde_json::Value = client.get(&url).send().await.unwrap().json().await.unwrap();
        assert_eq!(snapshot["link_status"], expected);
        assert_eq!(
            snapshot["prompts"],
            json!({"voice":"admitted voice","runner":"admitted runner"})
        );
        assert_eq!(snapshot["settings"]["agent"]["max_tool_calls"], 8);
    }
    assert_eq!(
        settings_runtime_patch(fixture, json!({"agent":{"max_tool_calls":8}})).await,
        json!({"version":1,"restart_pending":false})
    );
    let previous = std::fs::read(home.join("settings.toml")).unwrap();
    let invalid = client
        .post(&url)
        .json(&json!({"version":1,"settings":{"agent":{"max_tool_calls":33}}}))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(std::fs::read(home.join("settings.toml")).unwrap(), previous);
    let prompt = client
        .post(format!(
            "http://127.0.0.1:{}/settings/v1/prompts/voice?token=synthetic-screen-pair",
            fixture.screen.port()
        ))
        .json(&json!({"version":1,"text":"saved voice\n"}))
        .send()
        .await
        .unwrap();
    assert_eq!(prompt.status(), reqwest::StatusCode::OK);
    assert_eq!(
        prompt.json::<serde_json::Value>().await.unwrap(),
        json!({"version":1,"restart_pending":false})
    );
    assert_eq!(
        std::fs::read_to_string(home.join("prompts/voice.md")).unwrap(),
        "saved voice\n"
    );
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_configured_service_authenticates_both_modes_before_ready_and_reaps_on_cancel() {
    for mode in ["chatgpt", "api_key"] {
        let fixture = Fixture::new(mode);
        let config = config(&fixture);
        if mode == "api_key" {
            std::fs::write(&config.live.credential_file, "synthetic-nominated-key\r\n").unwrap();
        } else {
            // A directory would fail credential reading. ChatGPT must ignore it.
            std::fs::create_dir(&config.live.credential_file).unwrap();
        }
        let observer = Observer::default();
        let path = fixture.directory.path().join("configured-trace");
        let prepared = Configured::new(config, path.clone(), ([127, 0, 0, 1], 0).into()).unwrap();
        let (stop, stopping) = tokio::sync::oneshot::channel();
        let work = prepared.execute(
            FakeConnector::new(observer.clone()),
            async || -> Result<echo_media::Peer, Error> { unreachable!("no wake creates no media") },
            vec![],
            async {
                stopping.await.unwrap();
            },
        );
        let inspect = async {
            let _control = fixture.control().await;
            let mut screen = bridge_wire_screen::stream(&fixture.screen.to_string()).await;
            loop {
                let state = bridge_wire_screen::state(&mut screen).await;
                assert_eq!(state["state"], "ready");
                if state["message"] == "Echo Live ready" {
                    break;
                }
            }
            assert_eq!(
                std::fs::read_to_string(fixture.home.join("last-method")).unwrap(),
                "account/read"
            );
            assert_eq!(
                observer.commands(),
                [Command::Connect, Command::Capture, Command::Probe]
            );
            presentation_owned(&path);
            stop.send(()).unwrap();
        };
        let (result, ()) = Box::pin(tokio::time::timeout(OBSERVATION_TIMEOUT, async {
            tokio::join!(work, inspect)
        }))
        .await
        .unwrap();
        assert_eq!(result, Ok(()));
        assert_eq!(
            observer.commands(),
            [Command::Connect, Command::Capture, Command::Probe, Command::Close]
        );
        super::host::released(&fixture).await;
        presentation_released(&path, mode, true);
        let rows = trace(&path);
        assert_eq!(rows.iter().filter(|row| row["event"] == "bridge.ready").count(), 1);
        assert_eq!(rows.iter().filter(|row| row["event"] == "live.connected").count(), 0);
        assert_eq!(rows.iter().filter(|row| row["event"] == "run.started").count(), 0);
    }
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_configured_service_missing_binary_refuses_before_device_and_child() {
    let fixture = Fixture::new("chatgpt");
    let mut config = config(&fixture);
    config.codex.binary = Some(fixture.directory.path().join("missing-binary"));
    // Observe the address actually owned by this run rather than a released
    // fixture reservation that another parallel test may have reused.
    config.screen.listen = ([127, 0, 0, 1], 0).into();
    let path = fixture.directory.path().join("configured-trace");
    let observer = Observer::default();
    let prepared = Configured::new(config, path.clone(), ([127, 0, 0, 1], 0).into()).unwrap();
    let result = Box::pin(prepared.execute(
        FakeConnector::new(observer.clone()),
        async || -> Result<echo_media::Peer, Error> { unreachable!() },
        vec![],
        std::future::pending(),
    ))
    .await;
    assert_eq!(result, Err(Error::CodexBinary));
    assert_eq!(observer.commands(), []);
    assert!(!fixture.home.join("launches").exists());
    let rows = trace(&path);
    let screens: Vec<_> = rows.iter().filter(|row| row["event"] == "screen.listening").collect();
    assert_eq!(screens.len(), 1);
    presentation_released(&path, "chatgpt", true);
    assert_eq!(
        trace(&path).iter().filter(|row| row["event"] == "bridge.ready").count(),
        0
    );
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_configured_service_missing_nominated_key_creates_no_runtime_resources() {
    let fixture = Fixture::new("api_key");
    let mut config = config(&fixture);
    let screen = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    screen.set_nonblocking(true).unwrap();
    config.screen.listen = screen.local_addr().unwrap();
    let path = fixture.directory.path().join("configured-trace");
    let observer = Observer::default();
    let prepared = Configured::new(config, path.clone(), ([127, 0, 0, 1], 0).into()).unwrap();
    let result = Box::pin(prepared.execute(
        FakeConnector::new(observer.clone()),
        async || -> Result<echo_media::Peer, Error> { unreachable!() },
        vec![],
        std::future::pending(),
    ))
    .await;
    assert_eq!(result, Err(Error::Authorization));
    assert_eq!(observer.commands(), []);
    assert!(!path.exists());
    assert!(!fixture.home.join("launches").exists());
    assert_eq!(screen.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_configured_service_health_bind_failure_releases_only_acquired_screen() {
    let fixture = Fixture::new("chatgpt");
    let mut config = config(&fixture);
    config.screen.listen = ([127, 0, 0, 1], 0).into();
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let path = fixture.directory.path().join("configured-trace");
    let observer = Observer::default();
    let prepared = Configured::new(config, path.clone(), occupied.local_addr().unwrap()).unwrap();
    let result = Box::pin(prepared.execute(
        FakeConnector::new(observer.clone()),
        async || -> Result<echo_media::Peer, Error> { unreachable!() },
        vec![],
        std::future::pending(),
    ))
    .await;
    assert_eq!(result, Err(Error::Health));
    assert_eq!(observer.commands(), []);
    assert!(!fixture.home.join("launches").exists());
    let rows = trace(&path);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["event"], "screen.released");
    assert_eq!(rows[0]["provider_auth_mode"], "chatgpt");
    assert_eq!(rows[0].as_object().unwrap().len(), 4);
    assert_eq!(rows[0]["dropped_clients"], 0);
    assert!(rows[0]["elapsed_ms"].is_u64());
    // The refused health listener belongs to this test and remains live.
    let client = tokio::net::TcpStream::connect(occupied.local_addr().unwrap())
        .await
        .unwrap();
    let (server, _) = occupied.accept().await.unwrap();
    assert_eq!(client.local_addr().unwrap(), server.peer_addr().unwrap());
}

// This uses the delivered CLI and SSE endpoint, with the existing controlled
// account child and loopback Wyoming. A new fixture isolates each rejected file.
#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge and local Python"]
async fn t3_settings_overlay_invalid_startup_reports_and_ignores_entire_file() {
    for (overlay, reason) in [
        ("[voice", "settings.toml: invalid TOML syntax"),
        (
            "[voice]\nsession_listen_seconds=1\n",
            "voice.session_listen_seconds: must be within 3..=300",
        ),
    ] {
        let fixture = Fixture::new("chatgpt");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        super::voice_entry::configure(&fixture, listener.local_addr().unwrap().port());
        let base = fixture.directory.path().join("bridge.toml");
        let home = fixture.directory.path().join("agent-home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("settings.toml"), overlay).unwrap();
        let text = format!(
            "{}\n[home]\ndir='{}'\n",
            std::fs::read_to_string(&base).unwrap(),
            home.display()
        );
        std::fs::write(&base, &text).unwrap();
        let mut process = Process::spawn(&mut super::voice_entry::command(&fixture));
        let mut satellite = tokio::time::timeout(OBSERVATION_TIMEOUT, listener.accept())
            .await
            .unwrap()
            .unwrap()
            .0;
        assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "run-satellite");
        assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "describe");
        bridge_short::send(&mut satellite, "info", json!({"satellite":{"name":"synthetic"}})).await;
        assert_eq!(process.line().await, r#"{"event":"bridge.ready"}"#);
        let mut screen = bridge_wire_screen::stream(&fixture.screen.to_string()).await;
        assert_eq!(
            bridge_wire_screen::state(&mut screen).await,
            json!({
                "state":"error", "relink":false, "message": format!("settings.toml: ignored overlay: {reason}")
            })
        );
        assert_eq!(std::fs::read_to_string(&base).unwrap(), text);
        assert_eq!(std::fs::read_to_string(home.join("settings.toml")).unwrap(), overlay);
        drop(process.input.take());
        process.collect(0).await;
        assert_eq!(process.stderr().await, "");
        super::host::released(&fixture).await;
        let rows = trace(&fixture.directory.path().join("trace"));
        let warnings: Vec<_> = rows
            .iter()
            .filter(|row| row["event"] == "settings.overlay_ignored")
            .collect();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].as_object().unwrap().len(), 3);
        assert!(
            !serde_json::to_string(&rows)
                .unwrap()
                .contains(&home.display().to_string())
        );
    }
}
