//! Component T3: actual reqwest and private filesystem, synthetic 127.0.0.1 provider.
use super::auth_support::*;
use crate::{AuthError, auth_store};
use std::{collections::BTreeMap, io::Write};

#[tokio::test]
async fn t3_device_exact_requests_callback_and_reload() {
    let mock = Mock::start(vec![
        device(),
        (403, "{}".into()),
        (404, "{}".into()),
        approval(),
        (200, tokens(Some("synthetic-account"))),
    ])
    .await;
    let directory = directory();
    let source = mock.source(&directory);
    let mut callbacks = 0;
    let credential = source
        .device_sign_in(|prompt| {
            callbacks += 1;
            assert_eq!(mock.requests.lock().unwrap().len(), 1);
            assert_eq!(prompt.user_code, "TEST-CODE");
            assert_eq!(prompt.verification_url, format!("{}/verify", mock.base));
        })
        .await
        .unwrap();
    assert_eq!(callbacks, 1);
    assert_eq!(credential.account_id, "synthetic-account");
    assert_eq!(*credential.access_token.expose(), token(Some("synthetic-account")));
    let loaded = mock.source(&directory).current().await.unwrap();
    assert_eq!(*loaded.access_token.expose(), *credential.access_token.expose());
    let requests = mock.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        requests.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        ["/user", "/poll", "/poll", "/poll", "/token"]
    );
    assert_requests(&requests, &mock.base);
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(&source.file).unwrap()).unwrap();
    assert_eq!(
        stored,
        serde_json::json!({"access":token(Some("synthetic-account")),"refresh":"synthetic-refresh","expires":NOW+3600,"account_id":"synthetic-account","last_refresh":NOW})
    );
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
    mock.stop().await;
}

fn assert_requests(requests: &[Request], base: &str) {
    for request in requests {
        assert_eq!(request.headers["originator"], "openclaw");
        assert_eq!(request.headers["user-agent"], "openclaw");
        for absent in ["authorization", "dpop", "resource", "version"] {
            assert!(!request.headers.contains_key(absent));
        }
    }
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&requests[0].body).unwrap(),
        serde_json::json!({"client_id":"mock-client"})
    );
    for request in &requests[1..4] {
        assert_eq!(request.headers["content-type"], "application/json");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&request.body).unwrap(),
            serde_json::json!({"device_auth_id":"synthetic-device","user_code":"TEST-CODE"})
        );
    }
    assert_eq!(requests[4].headers["content-type"], "application/x-www-form-urlencoded");
    let form: BTreeMap<_, _> = reqwest::Url::parse(&format!("http://127.0.0.1/?{}", requests[4].body))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    assert_eq!(
        form,
        BTreeMap::from([
            ("grant_type".into(), "authorization_code".into()),
            ("client_id".into(), "mock-client".into()),
            ("code".into(), "synthetic-code".into()),
            ("code_verifier".into(), "synthetic-verifier".into()),
            ("redirect_uri".into(), format!("{base}/callback")),
        ])
    );
    writeln!(
        std::io::stdout(),
        "D1: callback=1; usercode=1; pending=2; polls=3; exchange=1; exact JSON/headers/form={form:?}; reload=current"
    )
    .unwrap();
}

#[tokio::test]
async fn t3_device_failures_leave_old_bytes_and_stop_requests() {
    let cases = [
        (vec![(200, "{}".into())], AuthError::InvalidCredential, 0),
        (
            vec![(200, r#"{"device_auth_id":"","user_code":"TEST-CODE"}"#.into())],
            AuthError::InvalidCredential,
            0,
        ),
        (vec![(404, "INJECTED_VALUE".into())], AuthError::AuthorizationDenied, 0),
        (
            vec![device(), (500, "INJECTED_VALUE".into())],
            AuthError::AuthorizationDenied,
            1,
        ),
        (vec![device(), (200, "{}".into())], AuthError::InvalidCredential, 1),
        (
            vec![
                device(),
                (200, r#"{"authorization_code":"test","code_verifier":""}"#.into()),
            ],
            AuthError::InvalidCredential,
            1,
        ),
        (vec![(200, "x".repeat(262_145))], AuthError::InvalidCredential, 0),
        (
            vec![device(), approval(), (400, "INJECTED_VALUE".into())],
            AuthError::RelinkRequired,
            1,
        ),
        (
            vec![device(), approval(), (200, tokens(None))],
            AuthError::AccountIdMissing,
            1,
        ),
    ];
    for (replies, expected, callbacks_expected) in cases {
        let count = replies.len();
        let mock = Mock::start(replies).await;
        let directory = directory();
        let source = mock.source(&directory);
        auth_store::save(&source.file, &record()).unwrap();
        let before = std::fs::read(&source.file).unwrap();
        let mut callbacks = 0;
        let err = source.device_sign_in(|_| callbacks += 1).await.err().unwrap();
        assert_eq!(err, expected);
        assert!(!format!("{err:?}: {err}").contains("INJECTED_VALUE"));
        assert_eq!(callbacks, callbacks_expected);
        assert_eq!(mock.requests.lock().unwrap().len(), count);
        assert_eq!(std::fs::read(&source.file).unwrap(), before);
        mock.stop().await;
    }
}

#[tokio::test]
async fn t3_device_transport_failure_preserves_existing_credential() {
    // A reserved loopback listener supplies a deterministic connection close,
    // rather than depending on a supposedly unused port remaining unused.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock = Mock::start(vec![]).await;
    let directory = directory();
    let mut source = mock.source(&directory);
    let mut endpoint = reqwest::Url::parse("http://127.0.0.1/user").unwrap();
    endpoint.set_port(Some(listener.local_addr().unwrap().port())).unwrap();
    source.config.usercode_endpoint = endpoint.into();
    auth_store::save(&source.file, &record()).unwrap();
    let before = std::fs::read(&source.file).unwrap();
    let peer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        drop(stream);
    });
    assert_eq!(
        source.device_sign_in(|_| unreachable!()).await.err(),
        Some(AuthError::Transport)
    );
    peer.await.unwrap();
    assert_eq!(std::fs::read(&source.file).unwrap(), before);
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
    assert_eq!(mock.requests.lock().unwrap().len(), 0);
    mock.stop().await;
}

#[tokio::test]
async fn t3_device_deadline_never_polls_at_900_seconds() {
    let mut replies = vec![device()];
    replies.extend((0..180).map(|_| (403, "{}".into())));
    let mock = Mock::start(replies).await;
    let directory = directory();
    let source = mock.source(&directory);
    assert_eq!(source.device_sign_in(|_| {}).await.err(), Some(AuthError::Timeout));
    assert_eq!(mock.requests.lock().unwrap().len(), 181);
    assert_eq!(source.clock.monotonic().as_secs(), 900);
    assert!(!source.file.exists());
    mock.stop().await;
}

#[tokio::test]
async fn t3_device_dropped_future_has_no_background_write_or_poll() {
    let mut mock = Mock::start(vec![(0, String::new())]).await;
    let directory = directory();
    let source = mock.source(&directory);
    let mut future = Box::pin(source.device_sign_in(|_| unreachable!()));
    tokio::select! {
        result = &mut future => { assert_eq!(result.err(),Some(AuthError::Cancelled)); }
        index = mock.seen.recv() => assert_eq!(index,Some(0)),
    }
    drop(future);
    assert_eq!(source.current().await.err(), Some(AuthError::NotSignedIn));
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
    assert!(!source.file.exists());
    mock.stop().await;
}

#[tokio::test]
async fn t3_device_request_timeout_is_bounded() {
    let mut mock = Mock::start(vec![(0, String::new())]).await;
    let directory = directory();
    let source = mock.source(&directory);
    let mut future = Box::pin(source.device_sign_in(|_| unreachable!()));
    tokio::select! {
        result = &mut future => { assert_eq!(result.err(),Some(AuthError::Timeout)); }
        index = mock.seen.recv() => assert_eq!(index,Some(0)),
    }
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    assert_eq!(future.await.err(), Some(AuthError::Timeout));
    assert!(!source.file.exists());
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
    mock.stop().await;
}
