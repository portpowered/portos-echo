//! T3 process boundary: one controlled child, no provider or hardware access.
use super::*;
use echo_bridge::{Error, bridge_codex::with_host};
use echo_codex::codex_supervisor::{ProcessError, Supervisor, SupervisorConfig};
use echo_config::{CodexConfig, LiveMode};
use echo_contracts::secret::Secret;
use tokio::time::Instant;

fn config(fixture: &Fixture) -> CodexConfig {
    CodexConfig {
        binary: Some(fixture.binary.clone()),
        home: Some(fixture.home.clone()),
        ..CodexConfig::default()
    }
}

async fn released(fixture: &Fixture) {
    drop(fixture.owner());
    assert!(
        tokio::net::TcpStream::connect((
            std::net::Ipv4Addr::LOCALHOST,
            std::fs::read_to_string(fixture.home.join("peer-port"))
                .unwrap()
                .parse::<u16>()
                .unwrap(),
        ))
        .await
        .is_err()
    );
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_host_admits_once_with_exclusive_initialized_authenticated_io_and_preserves_auth() {
    let fixture = Fixture::new("chatgpt");
    let auth = b"synthetic unchanged auth bytes\n";
    std::fs::write(fixture.home.join("auth.json"), auth).unwrap();
    let result = with_host(
        &config(&fixture),
        LiveMode::Chatgpt,
        None,
        Instant::now() + OBSERVATION_TIMEOUT,
        std::future::pending(),
        async |io| {
            assert_eq!(
                std::fs::read_to_string(fixture.home.join("last-method")).unwrap(),
                "account/read"
            );
            assert!(matches!(
                Supervisor::new(SupervisorConfig::new(fixture.binary.clone(), fixture.home.clone(),)),
                Err(ProcessError::PrivateHome)
            ));
            let id = io
                .begin_request("account/read", &json!({"refreshToken": false}))
                .unwrap();
            assert_eq!(io.wait_reply(&id).await.unwrap()["account"]["type"], "chatgpt");
            Ok(42)
        },
    )
    .await;
    assert_eq!(result, Ok(42));
    assert_eq!(std::fs::read(fixture.home.join("auth.json")).unwrap(), auth);
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("launches"))
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["started"]
    );
    released(&fixture).await;
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_host_nominated_api_key_and_account_mode_gate_session_admission() {
    for (peer_mode, mode, key, expected) in [
        ("api_key", LiveMode::ApiKey, true, Ok(())),
        ("api_key", LiveMode::ApiKey, false, Err(Error::Authorization)),
        ("api_key", LiveMode::Chatgpt, false, Err(Error::Authorization)),
        ("signed-out", LiveMode::Chatgpt, false, Err(Error::Authorization)),
        ("rejected-account", LiveMode::Chatgpt, false, Err(Error::Authorization)),
    ] {
        let fixture = Fixture::new(peer_mode);
        let key = key.then(|| Secret::new("synthetic-nominated-key".into()));
        let mut admissions = 0;
        let result = with_host(
            &config(&fixture),
            mode,
            key.as_ref(),
            Instant::now() + OBSERVATION_TIMEOUT,
            std::future::pending(),
            async |_| {
                admissions += 1;
                Ok(())
            },
        )
        .await;
        assert_eq!(result, expected);
        assert_eq!(admissions, usize::from(expected.is_ok()));
        released(&fixture).await;
    }
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_host_session_failure_and_cancellation_await_reap_before_releasing_home() {
    for cancel_session in [false, true] {
        let fixture = Fixture::new("chatgpt");
        let (entered, admission) = tokio::sync::oneshot::channel();
        let operation = with_host(
            &config(&fixture),
            LiveMode::Chatgpt,
            None,
            Instant::now() + OBSERVATION_TIMEOUT,
            async {
                if cancel_session {
                    admission.await.unwrap();
                } else {
                    std::future::pending::<()>().await;
                }
            },
            async |_| {
                entered.send(()).unwrap();
                if cancel_session {
                    std::future::pending::<()>().await;
                }
                Err::<(), _>(Error::Audio)
            },
        )
        .await;
        assert_eq!(
            operation,
            Err(if cancel_session { Error::Control } else { Error::Audio })
        );
        released(&fixture).await;
    }
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_host_cancelled_account_wait_never_admits_or_leaves_process_owner() {
    cancelled_request("blocked-account", "account/read").await;
    cancelled_request("blocked-initialize", "initialize").await;
}

async fn cancelled_request(mode: &str, method: &str) {
    let fixture = Fixture::new(mode);
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let settings = config(&fixture);
    let operation = with_host(
        &settings,
        LiveMode::Chatgpt,
        None,
        Instant::now() + OBSERVATION_TIMEOUT,
        async {
            cancelled.await.unwrap();
        },
        async |_| -> Result<(), Error> { unreachable!() },
    );
    let observer = async {
        let connection = fixture.control().await;
        loop {
            if std::fs::read_to_string(fixture.home.join("last-method")).is_ok_and(|m| m == method) {
                break;
            }
            tokio::task::yield_now().await;
        }
        cancel.send(()).unwrap();
        connection
    };
    let (result, connection) = tokio::time::timeout(OBSERVATION_TIMEOUT, async { tokio::join!(operation, observer) })
        .await
        .unwrap();
    assert_eq!(result, Err(Error::Control));
    drop(connection);
    released(&fixture).await;
}

#[tokio::test]
#[ignore = "T3 requires local Python"]
async fn codex_host_missing_binary_and_precancelled_admission_have_no_launch_or_home_effect() {
    let fixture = Fixture::new("chatgpt");
    let mut settings = config(&fixture);
    settings.home = Some(fixture.directory.path().join("untouched-home"));
    settings.binary = Some(fixture.directory.path().join("absent-binary"));
    for cancelled in [false, true] {
        let result = with_host(
            &settings,
            LiveMode::Chatgpt,
            None,
            Instant::now() + OBSERVATION_TIMEOUT,
            async {
                if !cancelled {
                    std::future::pending::<()>().await;
                }
            },
            async |_| -> Result<(), Error> { unreachable!() },
        )
        .await;
        assert_eq!(result, Err(if cancelled { Error::Control } else { Error::CodexBinary }));
        assert!(!settings.home.as_ref().unwrap().exists());
        assert!(!fixture.home.join("launches").exists());
    }
}
