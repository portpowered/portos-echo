//! Stored metadata and unchanged refresh intent; loopback listener detects any HTTP.
use crate::{AuthError, AuthSource, auth_private::Directory, auth_session, auth_store};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use echo_contracts::secret::Secret;
use std::{fs, io::ErrorKind};

fn record(expires: u64) -> auth_store::Record {
    let claims = serde_json::json!({
        "exp": 4_000_000_000_u64,
        "https://api.openai.com/auth": {"chatgpt_account_id": "synthetic-account-1234"}, // lint-rules: allow-literal inert JWT claim key
    });
    auth_store::Record {
        access: Secret::new(format!("e30.{}.synthetic", URL_SAFE_NO_PAD.encode(claims.to_string()))),
        refresh: Secret::new("synthetic-refresh".into()),
        expires,
        account_id: "synthetic-account-1234".into(),
        last_refresh: 1,
    }
}

#[tokio::test]
async fn t3_status_is_read_only_for_valid_due_and_expired_records() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut endpoint = reqwest::Url::parse("http://127.0.0.1/token").unwrap();
    endpoint.set_port(Some(listener.local_addr().unwrap().port())).unwrap();
    let config = echo_config::ChatGptAuthConfig {
        token_endpoint: endpoint.into(),
        ..Default::default()
    };
    let directory = Directory::new(&std::env::temp_dir()).unwrap();
    let path = directory.0.join("credential");
    let source = AuthSource::new(config, path.clone()).unwrap();
    for expires in [4_000_000_000, 2_000_000_001, 1] {
        auth_store::save(&path, &record(expires)).unwrap();
        let before = fs::read(&path).unwrap();
        let lock = auth_session::lock(&path).unwrap();
        auth_session::mark(&lock, true).unwrap();
        drop(lock);
        let status = source.status().await.unwrap();
        assert_eq!(status.account_id, "synthetic-account-1234");
        assert_eq!(status.expires, expires);
        assert_eq!(fs::read(&path).unwrap(), before);
        let lock = auth_session::lock(&path).unwrap();
        assert!(auth_session::pending(&lock).unwrap());
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    }
}

#[tokio::test]
async fn status_preserves_loader_error_categories() {
    let directory = Directory::new(&std::env::temp_dir()).unwrap();
    let path = directory.0.join("credential");
    let source = AuthSource::new(echo_config::ChatGptAuthConfig::default(), path.clone()).unwrap();
    assert_eq!(source.status().await.err(), Some(AuthError::NotSignedIn));
    for bytes in [b"malformed".to_vec(), vec![b' '; 262_145]] {
        fs::write(&path, bytes).unwrap();
        assert_eq!(source.status().await.err(), Some(AuthError::InvalidCredential));
    }
    let mut inconsistent = record(4_000_000_000);
    inconsistent.account_id = "different-synthetic-account".into();
    fs::write(&path, serde_json::to_vec(&inconsistent).unwrap()).unwrap();
    assert_eq!(source.status().await.err(), Some(AuthError::AccountMismatch));
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert_eq!(source.status().await.err(), Some(AuthError::Storage));
}

#[tokio::test]
async fn status_obeys_file_and_operation_locks_without_mutation() {
    let directory = Directory::new(&std::env::temp_dir()).unwrap();
    let path = directory.0.join("credential");
    auth_store::save(&path, &record(1)).unwrap();
    let source = AuthSource::new(echo_config::ChatGptAuthConfig::default(), path.clone()).unwrap();
    let file = auth_session::lock(&path).unwrap();
    assert_eq!(source.status().await.err(), Some(AuthError::Busy));
    drop(file);
    let operation = source.operation.lock().await;
    let status = source.status();
    tokio::pin!(status);
    assert!(
        std::future::poll_fn(|context| {
            std::task::Poll::Ready(std::future::Future::poll(status.as_mut(), context).is_pending())
        })
        .await
    );
    drop(operation);
    assert_eq!(status.await.unwrap().expires, 1);
    fs::remove_file(path.with_file_name("credential.lock")).unwrap();
    fs::create_dir(path.with_file_name("credential.lock")).unwrap();
    assert_eq!(source.status().await.err(), Some(AuthError::InvalidConfig));
}
