//! Component T3: real HTTP and store, synthetic loopback issuer and fake time.
use super::auth_support::*;
use crate::{AuthError, auth_store};
use std::{collections::BTreeMap, io::Write};

fn rotated() -> String {
    let mut body: serde_json::Value = serde_json::from_str(&tokens(Some("synthetic-account"))).unwrap();
    body["refresh_token"] = "synthetic-rotated".into();
    body["access_token"] = token(Some("synthetic-account")).replacen("e30.", "e31.", 1).into();
    body.to_string()
}
fn assert_form(request: &Request, refresh: &str) {
    assert_eq!(request.path, "/token");
    assert_eq!(request.headers["content-type"], "application/x-www-form-urlencoded");
    for absent in ["authorization", "originator", "dpop", "user-agent"] {
        assert!(!request.headers.contains_key(absent));
    }
    let fields: BTreeMap<_, _> = reqwest::Url::parse(&format!("http://127.0.0.1/?{}", request.body))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    assert_eq!(
        fields,
        BTreeMap::from([
            ("grant_type".into(), "refresh_token".into()),
            ("client_id".into(), "mock-client".into()),
            ("refresh_token".into(), refresh.into()),
        ])
    );
    writeln!(std::io::stdout(), "refresh exact form={fields:?}").unwrap();
}
#[tokio::test]
async fn t3_refresh_exact_boundaries_and_durable_rotation() {
    for (remaining, age, count) in [
        (301, 0, 0),
        (300, 0, 1),
        (299, 0, 1),
        (3600, 691_199, 0),
        (3600, 691_200, 0),
        (3600, 691_201, 1),
        (300, 691_201, 1),
    ] {
        let mock = Mock::start(vec![(200, rotated()), (200, rotated())]).await;
        let directory = directory();
        let source = mock.source(&directory);
        let mut old = record();
        old.expires = NOW + remaining;
        old.last_refresh = NOW - age;
        auth_store::save(&source.file, &old).unwrap();
        let credential = source.current().await.unwrap();
        assert_eq!(credential.account_id, "synthetic-account");
        let expected = if count == 0 {
            token(Some("synthetic-account"))
        } else {
            token(Some("synthetic-account")).replacen("e30.", "e31.", 1)
        };
        assert_eq!(*credential.access_token.expose(), expected);
        assert_eq!(mock.requests.lock().unwrap().len(), count);
        if count == 1 {
            assert_form(&mock.requests.lock().unwrap()[0], "synthetic-refresh");
            let mut stored = auth_store::load(&source.file).unwrap();
            assert_eq!(stored.refresh.expose(), "synthetic-rotated");
            assert_eq!(stored.last_refresh, NOW);
            assert_eq!(stored.expires, NOW + 3600);
            // Force the next due call; it must use the persisted rotation.
            stored.expires = NOW + 300;
            auth_store::save(&source.file, &stored).unwrap();
            source.current().await.unwrap();
            assert_form(&mock.requests.lock().unwrap()[1], "synthetic-rotated");
        }
        writeln!(
            std::io::stdout(),
            "boundary remaining={remaining}; age={age}; initial posts={count}"
        )
        .unwrap();
        mock.stop().await;
    }
}
#[tokio::test]
async fn t3_refresh_failure_preserves_bytes_and_does_not_retry() {
    let mut missing: serde_json::Value = serde_json::from_str(&rotated()).unwrap();
    missing.as_object_mut().unwrap().remove("refresh_token");
    let mut empty = missing.clone();
    empty["refresh_token"] = "".into();
    let mut null = missing.clone();
    null["refresh_token"] = serde_json::Value::Null;
    for (status, body, expected) in [
        (400, "injected".into(), Some(AuthError::RelinkRequired)),
        (200, "{}".into(), Some(AuthError::InvalidCredential)),
        (200, empty.to_string(), Some(AuthError::InvalidCredential)),
        (200, null.to_string(), Some(AuthError::InvalidCredential)),
        (200, tokens(Some("different-account")), Some(AuthError::AccountMismatch)),
        (200, missing.to_string(), None),
    ] {
        let mock = Mock::start(vec![(status, body)]).await;
        let directory = directory();
        let source = mock.source(&directory);
        let mut old = record();
        old.expires = NOW + 300;
        auth_store::save(&source.file, &old).unwrap();
        let before = std::fs::read(&source.file).unwrap();
        assert_eq!(source.current().await.err(), expected);
        assert_eq!(mock.requests.lock().unwrap().len(), 1);
        if expected.is_some() {
            assert_eq!(std::fs::read(&source.file).unwrap(), before);
            assert_eq!(
                mock.source(&directory).current().await.err(),
                Some(AuthError::RelinkRequired)
            );
            assert_eq!(mock.requests.lock().unwrap().len(), 1);
        } else {
            assert_eq!(
                auth_store::load(&source.file).unwrap().refresh.expose(),
                "synthetic-refresh"
            );
        }
        mock.stop().await;
    }
}
#[tokio::test]
async fn t3_refresh_concurrent_sources_and_shared_callers_never_double_rotate() {
    let mock = Mock::start(vec![(200, rotated())]).await;
    let directory = directory();
    let source = mock.source(&directory);
    let other = mock.source(&directory);
    let mut old = record();
    old.expires = NOW + 300;
    auth_store::save(&source.file, &old).unwrap();
    let held = crate::auth_session::lock(&source.file).unwrap();
    assert_eq!(other.current().await.err(), Some(AuthError::Busy));
    drop(held);
    let (first, second) = tokio::join!(source.current(), source.current());
    assert_eq!(
        *first.unwrap().access_token.expose(),
        *second.unwrap().access_token.expose()
    );
    assert_eq!(
        *other.current().await.unwrap().access_token.expose(),
        token(Some("synthetic-account")).replacen("e30.", "e31.", 1)
    );
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
    mock.stop().await;
}
#[tokio::test]
async fn t3_refresh_timeout_preserves_old_file() {
    let mut mock = Mock::start(vec![(0, String::new())]).await;
    let directory = directory();
    let source = mock.source(&directory);
    let mut old = record();
    old.expires = NOW + 300;
    auth_store::save(&source.file, &old).unwrap();
    let before = std::fs::read(&source.file).unwrap();
    let mut future = Box::pin(source.current());
    tokio::select! {
        result = &mut future => panic!("unexpected completion {:?}", result.err()),
        seen = mock.seen.recv() => assert_eq!(seen, Some(0)),
    }
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    assert_eq!(future.await.err(), Some(AuthError::Timeout));
    assert_eq!(std::fs::read(&source.file).unwrap(), before);
    assert_eq!(source.current().await.err(), Some(AuthError::RelinkRequired));
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
    mock.stop().await;
}

#[tokio::test]
async fn t3_refresh_cancelled_request_requires_relink_without_replay() {
    let mut mock = Mock::start(vec![(0, String::new())]).await;
    let directory = directory();
    let source = mock.source(&directory);
    let mut old = record();
    old.expires = NOW + 300;
    auth_store::save(&source.file, &old).unwrap();
    let before = std::fs::read(&source.file).unwrap();
    let mut future = Box::pin(source.current());
    tokio::select! {
        result = &mut future => panic!("unexpected completion {:?}", result.err()),
        seen = mock.seen.recv() => assert_eq!(seen, Some(0)),
    }
    drop(future);
    assert_eq!(
        mock.source(&directory).current().await.err(),
        Some(AuthError::RelinkRequired)
    );
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
    assert_eq!(std::fs::read(&source.file).unwrap(), before);
    mock.stop().await;
    let recovery = Mock::start(vec![device(), approval(), (200, tokens(Some("synthetic-account")))]).await;
    recovery.source(&directory).device_sign_in(|_| {}).await.unwrap();
    assert_eq!(
        recovery.source(&directory).current().await.unwrap().account_id,
        "synthetic-account"
    );
    assert_eq!(recovery.requests.lock().unwrap().len(), 3);
    assert_eq!(std::fs::read(directory.0.join("own.json.lock")).unwrap(), b"");
    recovery.stop().await;
}
