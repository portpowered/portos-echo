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
