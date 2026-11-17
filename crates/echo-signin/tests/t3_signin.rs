//! Component T3: public commands with real auth/client/store and a loopback issuer.
//! Each test owns a temporary directory and an ephemeral port; no real credentials.
#[cfg(test)]
#[path = "support/signin_browser.rs"]
mod signin_browser;
#[cfg(test)]
#[path = "support/signin_codex.rs"]
mod signin_codex;
#[cfg(test)]
#[path = "support/signin_codex_process.rs"]
mod signin_codex_process;
#[cfg(test)]
#[path = "support/signin_import.rs"]
mod signin_import;
#[cfg(test)]
mod support;

use echo_chatgpt_auth::{AuthError, AuthSource};
use echo_config::{ChatGptAuthConfig, LiveMode};
use echo_signin::{OperatorCommand, SigninError, resolve_mode};
use std::{fs, io::Write, time::Duration};
use support::*;

fn signin() -> OperatorCommand {
    OperatorCommand::Signin { browser: false }
}
fn no_secrets(bytes: &[u8]) {
    let text = String::from_utf8_lossy(bytes);
    for secret in [token(), REFRESH.into(), ACCOUNT.into()] {
        assert!(!text.contains(&secret));
    }
}

#[tokio::test]
async fn f01_f02_device_prompt_before_poll_commit_reload_and_read_only_status() {
    let mock = Mock::start(approved()).await;
    let directory = Directory::new().unwrap();
    let live = directory.live(LiveMode::Chatgpt);
    let providers = resolve_mode(live.clone(), mock.config.clone()).unwrap();
    let mut writer = Writer::new(&mock, usize::MAX);
    providers.execute(signin(), &mut writer).await.unwrap();
    let prompt = format!("code: TEST-CODE\nurl: {}\n", mock.config.verification_url);
    assert_eq!(writer.bytes, format!("{prompt}signed in\n").as_bytes());
    assert_eq!(
        *mock.events.lock().unwrap(),
        ["/user", "output", "/poll", "/token", "output"]
    );
    let loaded = AuthSource::new(mock.config.clone(), live.credential_file.clone())
        .unwrap()
        .current()
        .await
        .unwrap();
    assert_eq!(loaded.access_token.expose(), &token());
    assert_eq!(loaded.account_id, ACCOUNT);
    let before = fs::read(&live.credential_file).unwrap();
    let mut status = Vec::new();
    providers.execute(OperatorCommand::Status, &mut status).await.unwrap();
    assert_eq!(status, b"mode: chatgpt\naccount: ****1234\nexpires: 4000000000\n");
    assert_eq!(fs::read(&live.credential_file).unwrap(), before);
    {
        let requests = mock.requests.lock().unwrap();
        assert_eq!(
            requests.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
            ["/user", "/poll", "/token"]
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&requests[0].body).unwrap(),
            serde_json::json!({"client_id":"mock-client"})
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&requests[1].body).unwrap(),
            serde_json::json!({"device_auth_id":"synthetic-device","user_code":"TEST-CODE"})
        );
        assert_eq!(
            requests[2].body,
            format!(
                "grant_type=authorization_code&client_id=mock-client&code=synthetic-code&code_verifier=synthetic-verifier&redirect_uri={}",
                mock.config.device_redirect_uri.replace(':', "%3A").replace('/', "%2F")
            )
        );
    }
    no_secrets(&writer.bytes);
    no_secrets(&status);
    mock.stop().await;
    writeln!(
        std::io::stdout(),
        "F01/F02: exact output/order; exchanges=1; status HTTP delta=0; token matches=0"
    )
    .unwrap();
}

#[tokio::test]
async fn f03_status_safe_failures_do_not_emit_success() {
    let mock = Mock::start(vec![]).await;
    let directory = Directory::new().unwrap();
    let live = directory.live(LiveMode::Chatgpt);
    let providers = resolve_mode(live.clone(), mock.config.clone()).unwrap();
    let cases = [
        (None, AuthError::NotSignedIn),
        (Some(b"malformed".to_vec()), AuthError::InvalidCredential),
        (Some(vec![b' '; 262_145]), AuthError::InvalidCredential),
    ];
    for (bytes, expected) in cases {
        if let Some(bytes) = bytes {
            fs::write(&live.credential_file, bytes).unwrap();
        }
        let mut output = Vec::new();
        assert_eq!(
            providers.execute(OperatorCommand::Status, &mut output).await,
            Err(SigninError::Auth(expected))
        );
        assert_eq!(output, b"");
        no_secrets(expected.to_string().as_bytes());
    }
    assert_eq!(mock.requests.lock().unwrap().len(), 0);
    mock.stop().await;
    writeln!(
        std::io::stdout(),
        "F03: missing/invalid/oversized categories exact; output bytes=0; requests=0"
    )
    .unwrap();
}

#[tokio::test]
async fn f04_denial_disconnect_and_failed_commit_preserve_previous_bytes() {
    for (replies, expected, blocked_commit) in [
        (
            vec![(403, "synthetic-denial-body".into())],
            AuthError::AuthorizationDenied,
            false,
        ),
        (vec![(0, String::new())], AuthError::Transport, false),
        (approved(), AuthError::Storage, true),
    ] {
        let mock = Mock::start(replies).await;
        let directory = Directory::new().unwrap();
        let live = directory.live(LiveMode::Chatgpt);
        if blocked_commit {
            fs::create_dir(&live.credential_file).unwrap();
        } else {
            fs::write(&live.credential_file, b"previous bytes").unwrap();
        }
        let providers = resolve_mode(live.clone(), mock.config.clone()).unwrap();
        let mut output = Vec::new();
        assert_eq!(
            providers.execute(signin(), &mut output).await,
            Err(SigninError::Auth(expected))
        );
        assert!(!String::from_utf8_lossy(&output).contains("signed in"));
        if blocked_commit {
            assert_eq!(fs::read_dir(&live.credential_file).unwrap().count(), 0);
        } else {
            assert_eq!(fs::read(&live.credential_file).unwrap(), b"previous bytes");
        }
        no_secrets(&output);
        assert!(!String::from_utf8_lossy(&output).contains("synthetic-denial-body"));
        mock.stop().await;
    }
    writeln!(
        std::io::stdout(),
        "F04: denial/transport/storage exact; false success lines=0; previous state preserved"
    )
    .unwrap();
}

#[tokio::test]
async fn f04_stalled_request_timeout_and_drop_release_auth_locks() {
    for cancel in [false, true] {
        let mock = Mock::start(vec![(1, String::new())]).await;
        let directory = Directory::new().unwrap();
        let live = directory.live(LiveMode::Chatgpt);
        fs::write(&live.credential_file, b"previous bytes").unwrap();
        let providers = resolve_mode(live.clone(), mock.config.clone()).unwrap();
        let operation = async {
            let mut output = Vec::new();
            let result = providers.execute(signin(), &mut output).await;
            (result, output)
        };
        let mut task = Box::pin(operation);
        let mut mock = mock;
        tokio::select! {
            biased;
            _result = &mut task => panic!("stalled operation completed"),
            seen = mock.seen.recv() => assert_eq!(seen, Some(0)),
        }
        if cancel {
            drop(task);
        } else {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(31)).await;
            let (result, output) = task.await;
            tokio::time::resume();
            assert_eq!(result, Err(SigninError::Auth(AuthError::Timeout)));
            assert_eq!(output, b"");
        }
        assert_eq!(fs::read(&live.credential_file).unwrap(), b"previous bytes");
        let mut status = Vec::new();
        assert_eq!(
            providers.execute(OperatorCommand::Status, &mut status).await,
            Err(SigninError::Auth(AuthError::InvalidCredential))
        );
        assert_eq!(mock.requests.lock().unwrap().len(), 1);
        mock.stop().await;
    }
    writeln!(
        std::io::stdout(),
        "F04: fake-clock timeout and cancellation; requests=1 each; locks released"
    )
    .unwrap();
}

#[tokio::test]
async fn f05_failed_success_write_leaves_complete_committed_credential() {
    let mock = Mock::start(approved()).await;
    let directory = Directory::new().unwrap();
    let live = directory.live(LiveMode::Chatgpt);
    let providers = resolve_mode(live.clone(), mock.config.clone()).unwrap();
    let prompt = format!("code: TEST-CODE\nurl: {}\n", mock.config.verification_url);
    let mut output = Writer::new(&mock, prompt.len());
    assert_eq!(providers.execute(signin(), &mut output).await, Err(SigninError::Output));
    assert_eq!(output.bytes, prompt.as_bytes());
    let stored = AuthSource::new(mock.config.clone(), live.credential_file)
        .unwrap()
        .current()
        .await
        .unwrap();
    assert_eq!(stored.access_token.expose(), &token());
    assert_eq!(stored.account_id, ACCOUNT);
    no_secrets(&output.bytes);
    assert_eq!(SigninError::Output.to_string(), "command output failed");
    mock.stop().await;
    writeln!(
        std::io::stdout(),
        "F05: output error exact; complete credential reload matches; token matches=0"
    )
    .unwrap();
}

#[tokio::test]
async fn prompt_writer_failure_cancels_before_persistence() {
    let mock = Mock::start(approved()).await;
    let directory = Directory::new().unwrap();
    let live = directory.live(LiveMode::Chatgpt);
    let providers = resolve_mode(live.clone(), mock.config.clone()).unwrap();
    let mut output = Writer::new(&mock, 0);
    assert_eq!(providers.execute(signin(), &mut output).await, Err(SigninError::Output));
    assert_eq!(output.bytes, b"");
    assert!(!live.credential_file.exists());
    mock.stop().await;
}

#[tokio::test]
async fn api_key_status_and_resolver_never_open_credential_file() {
    let directory = Directory::new().unwrap();
    let live = directory.live(LiveMode::ApiKey);
    let providers = resolve_mode(live.clone(), ChatGptAuthConfig::default()).unwrap();
    assert_eq!(providers.mode(), LiveMode::ApiKey);
    assert!(!*providers.subscribe_kill().borrow());
    let mut output = Vec::new();
    providers.execute(OperatorCommand::Status, &mut output).await.unwrap();
    assert_eq!(output, b"mode: api-key\naccount: n/a\nexpires: n/a\n");
    assert_eq!(
        providers.execute(signin(), &mut Vec::new()).await,
        Err(SigninError::UnsupportedMode)
    );
    assert!(!live.credential_file.exists());
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 0);
}
