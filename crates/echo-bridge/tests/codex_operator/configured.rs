//! T3: config through authenticated service; controlled process and device ports.
use super::*;
use echo_bridge::{Error, bridge_voice::Configured};
use echo_link::fake::{Command, FakeConnector, Observer};

fn config(fixture: &Fixture) -> echo_config::BridgeConfig {
    let mut config = echo_config::BridgeConfig::load(fixture.directory.path().join("bridge.toml")).unwrap();
    config.audit.log_dir = fixture.directory.path().join("audit");
    config.live.credential_file = fixture.directory.path().join("nominated-key");
    config
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
    let address = screens[0]["address"].as_str().unwrap();
    let rebound = std::net::TcpListener::bind(address).unwrap();
    assert_eq!(rebound.local_addr().unwrap().to_string(), address);
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
