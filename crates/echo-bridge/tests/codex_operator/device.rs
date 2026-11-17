//! T3 controlled process plus in-memory device; not a milestone witness.
use super::*;
use echo_bridge::{
    Error,
    bridge_voice::{HostOptions, with_device},
};
use echo_config::{CodexConfig, LiveMode};
use echo_link::{
    LinkConnector,
    fake::{Command, FakeConnector, Observer},
};
use tokio::time::Instant;

fn runtime_options<'a>(
    config: &'a CodexConfig,
    screen: Option<&'a echo_config::ScreenConfig>,
    trace: &'a Path,
) -> echo_bridge::bridge_voice::RuntimeOptions<'a> {
    use echo_bridge::{bridge_codex, bridge_voice};
    bridge_voice::RuntimeOptions {
        host: options(config),
        voice: bridge_voice::ServeOptions {
            kill_path: None,
            call: bridge_codex::Options {
                voice: Some("alloy"),
                instructions: "synthetic fixture",
                model: None,
            },
            tools: vec![],
            session: bridge_voice::SessionOptions {
                idle: std::time::Duration::from_secs(1),
                response_timeout: OBSERVATION_TIMEOUT,
                played_timeout: OBSERVATION_TIMEOUT,
                reply_lead_ms: 200,
                muted: false,
            },
            connect_timeout: OBSERVATION_TIMEOUT,
        },
        screen,
        health: ([127, 0, 0, 1], 0).into(),
        trace,
    }
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_voice_runtime_prewake_ready_and_cancel_reap_before_presentation_closes() {
    let fixture = Fixture::new("chatgpt");
    let config = config(&fixture);
    let screen_config = echo_config::BridgeConfig::load(fixture.directory.path().join("bridge.toml"))
        .unwrap()
        .screen;
    let observer = Observer::default();
    let path = fixture.directory.path().join("runtime-trace");
    let (stop, stopping) = tokio::sync::oneshot::channel();
    let work = echo_bridge::bridge_voice::execute(
        runtime_options(&config, Some(&screen_config), &path),
        Connector {
            fixture: &fixture,
            observer: observer.clone(),
            fail: false,
        },
        async || -> Result<echo_media::Peer, Error> { unreachable!("no wake admits no peer") },
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
            observer.commands(),
            [Command::Connect, Command::Capture, Command::Probe]
        );
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
    assert!(tokio::net::TcpStream::connect(fixture.screen).await.is_err());
    assert_eq!(
        trace(&path).iter().filter(|row| row["event"] == "bridge.ready").count(),
        1
    );
    assert_eq!(
        trace(&path).iter().filter(|row| row["event"] == "run.started").count(),
        0
    );
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_voice_runtime_relink_stays_visible_without_device_or_lease_until_stop() {
    let fixture = Fixture::new("chatgpt");
    std::fs::write(fixture.home.join("mode"), "rejected-account").unwrap();
    let config = config(&fixture);
    let screen_config = echo_config::BridgeConfig::load(fixture.directory.path().join("bridge.toml"))
        .unwrap()
        .screen;
    let observer = Observer::default();
    let path = fixture.directory.path().join("runtime-trace");
    let (stop, stopping) = tokio::sync::oneshot::channel();
    let work = echo_bridge::bridge_voice::execute(
        runtime_options(&config, Some(&screen_config), &path),
        FakeConnector::new(observer.clone()),
        async || -> Result<echo_media::Peer, Error> { unreachable!() },
        async {
            stopping.await.unwrap();
        },
    );
    let inspect = async {
        let _control = fixture.control().await;
        let mut screen = bridge_wire_screen::stream(&fixture.screen.to_string()).await;
        loop {
            let state = bridge_wire_screen::state(&mut screen).await;
            if state["relink"] == true {
                assert_eq!(state["state"], "error");
                assert_eq!(
                    state["message"],
                    "Run echo-bridge signin with your config, then restart the bridge."
                );
                break;
            }
        }
        super::host::released(&fixture).await;
        assert_eq!(observer.commands(), []);
        stop.send(()).unwrap();
    };
    let (result, ()) = Box::pin(tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        tokio::join!(work, inspect)
    }))
    .await
    .unwrap();
    assert_eq!(result, Err(Error::Authorization));
    assert!(tokio::net::TcpStream::connect(fixture.screen).await.is_err());
    let rows = trace(&path);
    assert_eq!(
        rows.iter().filter(|row| row["event"] == "auth.relink_required").count(),
        1
    );
    assert_eq!(rows.iter().filter(|row| row["event"] == "bridge.ready").count(), 0);
    assert!(
        !std::fs::read_to_string(path)
            .unwrap()
            .contains("synthetic private error")
    );
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_voice_runtime_missing_binary_has_zero_child_device_and_call_effects() {
    let fixture = Fixture::new("chatgpt");
    let mut config = config(&fixture);
    config.binary = Some(fixture.directory.path().join("absent-binary"));
    let observer = Observer::default();
    let path = fixture.directory.path().join("runtime-trace");
    let result = Box::pin(echo_bridge::bridge_voice::execute(
        runtime_options(&config, None, &path),
        FakeConnector::new(observer.clone()),
        async || -> Result<echo_media::Peer, Error> { unreachable!() },
        std::future::pending(),
    ))
    .await;
    assert_eq!(result, Err(Error::CodexBinary));
    assert_eq!(observer.commands(), []);
    assert!(!fixture.home.join("launches").exists());
    assert_eq!(
        trace(&path).iter().filter(|row| row["event"] == "bridge.ready").count(),
        0
    );
    let health_address = trace(&path)[0]["health_addr"].as_str().unwrap().to_owned();
    assert!(tokio::net::TcpStream::connect(health_address).await.is_err());
}

fn config(fixture: &Fixture) -> CodexConfig {
    CodexConfig {
        binary: Some(fixture.binary.clone()),
        home: Some(fixture.home.clone()),
        ..CodexConfig::default()
    }
}

fn options(config: &CodexConfig) -> HostOptions<'_> {
    HostOptions {
        config,
        mode: LiveMode::Chatgpt,
        api_key: None,
        deadline: Instant::now() + OBSERVATION_TIMEOUT,
    }
}

struct Connector<'a> {
    fixture: &'a Fixture,
    observer: Observer,
    fail: bool,
}

impl LinkConnector for Connector<'_> {
    type Link = echo_link::fake::FakeLink;

    async fn connect(&self) -> Result<Self::Link, echo_link::Error> {
        // Device dial is forbidden until initialization/account admission.
        assert_eq!(
            std::fs::read_to_string(self.fixture.home.join("last-method")).unwrap(),
            "account/read"
        );
        assert!(matches!(
            echo_codex::codex_supervisor::Supervisor::new(echo_codex::codex_supervisor::SupervisorConfig::new(
                self.fixture.binary.clone(),
                self.fixture.home.clone()
            )),
            Err(echo_codex::codex_supervisor::ProcessError::PrivateHome)
        ));
        if self.fail {
            return Err(echo_link::Error::Transport);
        }
        FakeConnector::new(self.observer.clone()).connect().await
    }
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_device_auth_precedes_connect_and_failure_always_closes_device_and_child() {
    for fail_session in [false, true] {
        let fixture = Fixture::new("chatgpt");
        let config = config(&fixture);
        let observer = Observer::default();
        let result = with_device(
            options(&config),
            Connector {
                fixture: &fixture,
                observer: observer.clone(),
                fail: false,
            },
            std::future::pending(),
            async |io, _, stopping| {
                assert!(!*stopping.borrow());
                assert_eq!(
                    observer.commands(),
                    [Command::Connect, Command::Capture, Command::Probe]
                );
                let id = io
                    .begin_request("account/read", &json!({"refreshToken": false}))
                    .unwrap();
                assert_eq!(io.wait_reply(&id).await.unwrap()["account"]["type"], "chatgpt");
                if fail_session { Err(Error::Audio) } else { Ok(42) }
            },
        )
        .await;
        assert_eq!(result, if fail_session { Err(Error::Audio) } else { Ok(42) });
        assert_eq!(
            observer.commands(),
            [Command::Connect, Command::Capture, Command::Probe, Command::Close]
        );
        super::host::released(&fixture).await;
    }
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_device_cancellation_keeps_io_and_lease_until_callback_cleanup_then_closes() {
    let fixture = Fixture::new("chatgpt");
    let config = config(&fixture);
    let observer = Observer::default();
    let (entered, admission) = tokio::sync::oneshot::channel();
    let result = with_device(
        options(&config),
        FakeConnector::new(observer.clone()),
        async {
            admission.await.unwrap();
        },
        async |io, _, mut stopping| -> Result<(), Error> {
            entered.send(()).unwrap();
            while !*stopping.borrow_and_update() {
                stopping.changed().await.unwrap();
            }
            // Graceful session cleanup can still use the sole IO borrower.
            let id = io
                .begin_request("account/read", &json!({"refreshToken": false}))
                .unwrap();
            assert_eq!(io.wait_reply(&id).await.unwrap()["account"]["type"], "chatgpt");
            assert!(!observer.commands().contains(&Command::Close));
            assert!(matches!(
                echo_codex::codex_supervisor::Supervisor::new(echo_codex::codex_supervisor::SupervisorConfig::new(
                    fixture.binary.clone(),
                    fixture.home.clone()
                )),
                Err(echo_codex::codex_supervisor::ProcessError::PrivateHome)
            ));
            Err(Error::Control)
        },
    )
    .await;
    assert_eq!(result, Err(Error::Control));
    assert_eq!(
        observer.commands(),
        [Command::Connect, Command::Capture, Command::Probe, Command::Close]
    );
    super::host::released(&fixture).await;
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_device_rejected_account_or_missing_binary_never_dials() {
    for mode in ["signed-out", "rejected-account", "missing-binary"] {
        let fixture = Fixture::new(if mode == "missing-binary" { "chatgpt" } else { mode });
        let mut config = config(&fixture);
        if mode == "missing-binary" {
            config.binary = Some(fixture.directory.path().join("absent"));
        }
        let observer = Observer::default();
        let result = with_device(
            options(&config),
            FakeConnector::new(observer.clone()),
            std::future::pending(),
            async |_, _, _| -> Result<(), Error> { unreachable!() },
        )
        .await;
        assert_eq!(
            result,
            Err(if mode == "missing-binary" {
                Error::CodexBinary
            } else {
                Error::Authorization
            })
        );
        assert_eq!(observer.commands(), []);
        if mode == "missing-binary" {
            assert!(!fixture.home.join("launches").exists());
        } else {
            super::host::released(&fixture).await;
        }
    }
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_device_connect_failure_reaps_and_precancelled_start_has_zero_effects() {
    let fixture = Fixture::new("chatgpt");
    let config = config(&fixture);
    let observer = Observer::default();
    let result = with_device(
        options(&config),
        Connector {
            fixture: &fixture,
            observer: observer.clone(),
            fail: true,
        },
        std::future::pending(),
        async |_, _, _| -> Result<(), Error> { unreachable!() },
    )
    .await;
    assert_eq!(result, Err(Error::Wyoming));
    assert_eq!(observer.commands(), []);
    super::host::released(&fixture).await;
    let launches = std::fs::read(fixture.home.join("launches")).unwrap();
    let result = with_device(
        options(&config),
        FakeConnector::new(observer.clone()),
        async {},
        async |_, _, _| -> Result<(), Error> { unreachable!() },
    )
    .await;
    assert_eq!(result, Err(Error::Control));
    assert_eq!(observer.commands(), []);
    assert_eq!(std::fs::read(fixture.home.join("launches")).unwrap(), launches);
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_voice_runtime_kill_overlay_clear_and_cancel_keep_one_owned_process() {
    let fixture = Fixture::new("chatgpt");
    let config = config(&fixture);
    let screen_config = echo_config::BridgeConfig::load(fixture.directory.path().join("bridge.toml"))
        .unwrap()
        .screen;
    let observer = Observer::default();
    let trace_path = fixture.directory.path().join("runtime-trace");
    let kill_path = fixture.directory.path().join("bridge-kill");
    std::fs::write(&kill_path, []).unwrap();
    let mut options = runtime_options(&config, Some(&screen_config), &trace_path);
    options.voice.kill_path = Some(&kill_path);
    let (stop, stopping) = tokio::sync::oneshot::channel();
    let work = echo_bridge::bridge_voice::execute(
        options,
        FakeConnector::new(observer.clone()),
        async || -> Result<echo_media::Peer, Error> { unreachable!("no wake, no peer") },
        async {
            stopping.await.unwrap();
        },
    );
    let inspect = async {
        let _control = fixture.control().await;
        clear_kill_screen(&fixture, &kill_path).await;
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
    for event in ["kill.enabled", "kill.cleared"] {
        assert_eq!(trace(&trace_path).iter().filter(|row| row["event"] == event).count(), 1);
    }
    assert_eq!(
        trace(&trace_path)
            .iter()
            .filter(|row| row["event"] == "run.started")
            .count(),
        0
    );
}

async fn clear_kill_screen(fixture: &Fixture, kill_path: &Path) {
    let mut screen = bridge_wire_screen::stream(&fixture.screen.to_string()).await;
    loop {
        let state = bridge_wire_screen::state(&mut screen).await;
        if state["state"] == "error" {
            assert_eq!(
                state["message"],
                "Kill switch enabled. Run echo-bridge kill --clear with your config."
            );
            assert_eq!(state["relink"], false);
            break;
        }
    }
    std::fs::remove_file(kill_path).unwrap();
    loop {
        let state = bridge_wire_screen::state(&mut screen).await;
        if state["state"] == "ready" {
            assert_eq!(state["message"], "Echo Live ready");
            break;
        }
        assert_eq!(state["state"], "error");
    }
}
