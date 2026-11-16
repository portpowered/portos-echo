//! Component T3: public providers and real runtime consumers, synthetic loopback only.
#![cfg(test)]
#[cfg(test)]
#[path = "support/provider_runtime.rs"]
mod runtime;
#[cfg(test)]
mod support;
use echo_agent_loop::ModelCredential;
use echo_config::LiveMode;
use echo_live::call::{CallError, CredentialProvider, LiveCredential};
use echo_signin::{OperatorCommand, Providers, resolve_mode};
use std::{fs, io::Write};
use support::*;

async fn setup(mock: &Mock, directory: &Directory) -> Providers {
    let providers = resolve_mode(directory.live(LiveMode::Chatgpt), mock.config.clone()).unwrap();
    let mut writer = Writer::new(mock, usize::MAX);
    providers
        .execute(OperatorCommand::Signin { browser: false }, &mut writer)
        .await
        .unwrap();
    assert_eq!(
        writer.bytes,
        format!("code: TEST-CODE\nurl: {}\nsigned in\n", mock.config.verification_url).as_bytes()
    );
    assert!(!String::from_utf8_lossy(&writer.bytes).contains(&token()));
    providers
}
async fn kill(providers: &Providers, enabled: bool) {
    let mut writer = Vec::new();
    providers
        .execute(OperatorCommand::KillSwitch { enabled }, &mut writer)
        .await
        .unwrap();
    assert_eq!(
        writer,
        format!("kill-switch: {}\n", if enabled { "on" } else { "off" }).as_bytes()
    );
}
async fn refused(providers: &Providers) {
    assert!(matches!(providers.credential().await, Err(CallError::Authorization)));
    assert!(providers.headers(providers.mode()).await.is_err());
}
fn due(directory: &Directory, config: echo_config::ChatGptAuthConfig) -> Providers {
    let source = directory.0.join("explicit-import");
    fs::write(&source, serde_json::to_vec(&serde_json::json!({"tokens":{"access_token":token(), "refresh_token":REFRESH,"account_id":ACCOUNT}, "last_refresh":"2000-01-01T00:00:00Z"})).unwrap()).unwrap();
    resolve_mode(directory.live(LiveMode::Chatgpt), config).unwrap()
}
async fn import(providers: &Providers, directory: &Directory) {
    let mut output = Vec::new();
    providers
        .execute(
            OperatorCommand::ImportCodex {
                path: directory.0.join("explicit-import"),
            },
            &mut output,
        )
        .await
        .unwrap();
    assert_eq!(output, b"imported\n");
}

#[tokio::test(start_paused = true)]
async fn f11_device_signin_reaches_both_real_consumers() {
    let mock = Mock::start(approved()).await;
    let directory = Directory::new().unwrap();
    let providers = setup(&mock, &directory).await;
    let clone = providers.clone();
    let mut signal = clone.subscribe_kill();
    kill(&providers, true).await;
    signal.changed().await.unwrap();
    assert!(*signal.borrow_and_update());
    refused(&clone).await;
    assert_eq!(mock.requests.lock().unwrap().len(), 3);
    kill(&clone, true).await;
    assert!(!signal.has_changed().unwrap());
    kill(&clone, false).await;
    signal.changed().await.unwrap();
    assert!(!*signal.borrow_and_update());
    kill(&clone, false).await;
    assert!(!signal.has_changed().unwrap());
    runtime::live(&providers, &token(), Some(ACCOUNT)).await;
    runtime::model(&providers, &token(), Some(ACCOUNT)).await;
    assert_eq!(mock.requests.lock().unwrap().len(), 3);
    mock.stop().await;
    writeln!(
        std::io::stdout(),
        "F11/F15: ChatGPT shared kill on/off; Live POST=1, model POST=1; exact routes/models/headers; Completed(hello)"
    )
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn f12_f15_api_key_newlines_status_and_shared_kill() {
    for ending in ["\n", "\r\n"] {
        let directory = Directory::new().unwrap();
        let config = directory.live(LiveMode::ApiKey);
        fs::write(&config.credential_file, format!("synthetic-file-key{ending}")).unwrap();
        let providers = resolve_mode(config.clone(), echo_config::ChatGptAuthConfig::default()).unwrap();
        let clone = providers.clone();
        let mut signal = clone.subscribe_kill();
        kill(&providers, true).await;
        signal.changed().await.unwrap();
        assert!(*signal.borrow_and_update());
        fs::remove_file(&config.credential_file).unwrap();
        refused(&clone).await;
        kill(&clone, true).await;
        assert!(!signal.has_changed().unwrap());
        kill(&clone, false).await;
        signal.changed().await.unwrap();
        assert!(!*signal.borrow_and_update());
        kill(&clone, false).await;
        assert!(!signal.has_changed().unwrap());
        fs::write(&config.credential_file, format!("synthetic-file-key{ending}")).unwrap();
        runtime::live(&providers, "synthetic-file-key", None).await;
        runtime::model(&clone, "synthetic-file-key", None).await;
        let mut status = Vec::new();
        providers.execute(OperatorCommand::Status, &mut status).await.unwrap();
        assert_eq!(status, b"mode: api-key\naccount: n/a\nexpires: n/a\n");
        assert!(!String::from_utf8_lossy(&status).contains("synthetic-file-key"));
    }
    writeln!(
        std::io::stdout(),
        "F12/F15: LF+CRLF exact receiver headers; no account; status key matches=0; shared idempotent on/off"
    )
    .unwrap();
}

#[tokio::test]
async fn f13_invalid_key_and_mode_mismatch_refuse_before_network() {
    let directory = Directory::new().unwrap();
    let config = directory.live(LiveMode::ApiKey);
    let providers = resolve_mode(config.clone(), echo_config::ChatGptAuthConfig::default()).unwrap();
    refused(&providers).await;
    for bytes in [b"".as_slice(), b" \n", &[255], b"bad\nheader", b"bad\r", b"bad\0"] {
        fs::write(&config.credential_file, bytes).unwrap();
        refused(&providers).await;
    }
    let mock = Mock::start(vec![]).await;
    let chatgpt = resolve_mode(directory.live(LiveMode::Chatgpt), mock.config.clone()).unwrap();
    assert!(chatgpt.headers(LiveMode::ApiKey).await.is_err());
    assert_eq!(mock.requests.lock().unwrap().len(), 0);
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    mock.stop().await;
    writeln!(
        std::io::stdout(),
        "F13: missing/blank/UTF8/control keys refuse; mode mismatch before auth file/HTTP"
    )
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn f14_revoked_refresh_not_replayed_and_signin_recovers() {
    let mut replies = vec![(400, r#"{"error":"invalid_grant"}"#.into())];
    replies.extend(approved());
    let mock = Mock::start(replies).await;
    let directory = Directory::new().unwrap();
    let providers = due(&directory, mock.config.clone());
    import(&providers, &directory).await;
    refused(&providers).await;
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
    assert_eq!(
        mock.requests.lock().unwrap()[0].body,
        "grant_type=refresh_token&refresh_token=synthetic-refresh&client_id=mock-client"
    );
    providers
        .execute(OperatorCommand::Signin { browser: false }, &mut Vec::new())
        .await
        .unwrap();
    let credential = providers.credential().await.unwrap();
    assert_chatgpt(credential, &token());
    assert_eq!(mock.requests.lock().unwrap().len(), 4);
    mock.stop().await;
    writeln!(
        std::io::stdout(),
        "F14: revoked refresh POST=1 across Live+model; no replay; explicit signin recovers"
    )
    .unwrap();
}

#[cfg(test)]
fn assert_chatgpt(credential: LiveCredential, expected: &str) {
    assert!(matches!(&credential, LiveCredential::Chatgpt { .. }));
    if let LiveCredential::Chatgpt { token, account_id } = credential {
        assert!(token.expose().eq(expected));
        assert!(account_id.eq(ACCOUNT));
    }
}

#[tokio::test]
async fn f16_kill_cancels_stalled_refresh_and_prevents_replay() {
    let mut mock = Mock::start(vec![(1, String::new())]).await;
    let directory = Directory::new().unwrap();
    let providers = due(&directory, mock.config.clone());
    import(&providers, &directory).await;
    let clone = providers.clone();
    let mut pending = Box::pin(providers.credential());
    tokio::select! {
        biased;
        _ = &mut pending => panic!("stalled refresh returned"),
        seen = mock.seen.recv() => assert_eq!(seen, Some(0)),
    }
    let mut signal = clone.subscribe_kill();
    kill(&clone, true).await;
    signal.changed().await.unwrap();
    assert!(*signal.borrow());
    assert!(matches!(pending.await, Err(CallError::Authorization)));
    refused(&clone).await;
    kill(&clone, false).await;
    refused(&providers).await;
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
    mock.stop().await;
    writeln!(
        std::io::stdout(),
        "F16: pending refresh cancelled; host notified; both refuse; clear does not replay uncertain refresh"
    )
    .unwrap();
}

#[tokio::test]
async fn f18_shared_source_serializes_concurrent_refresh_and_persists_rotation() {
    let rotated = token().replace(".synthetic", ".rotated");
    let mock = Mock::start(vec![(
        200,
        serde_json::json!({"access_token":rotated,"refresh_token":"synthetic-rotated"}).to_string(),
    )])
    .await;
    let directory = Directory::new().unwrap();
    let providers = due(&directory, mock.config.clone());
    import(&providers, &directory).await;
    let clone = providers.clone();
    let (live, model) = tokio::join!(providers.credential(), clone.headers(LiveMode::Chatgpt));
    assert_chatgpt(live.unwrap(), &rotated);
    let model = model.unwrap();
    assert!(model.expose()["authorization"].eq(&format!("Bearer {rotated}")));
    assert!(model.expose()["chatgpt-account-id"].eq(ACCOUNT));
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
    let stored: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.live(LiveMode::Chatgpt).credential_file).unwrap()).unwrap();
    assert!(stored["access"].eq(&rotated));
    assert!(stored["refresh"].eq("synthetic-rotated"));
    mock.stop().await;
    writeln!(
        std::io::stdout(),
        "F18: concurrent Live+model refresh POST=1; both rotated values match; durable rotation exact"
    )
    .unwrap();
}
