//! T3 real public source and callback listener, synthetic loopback issuer only.
use super::auth_support::*;
use crate::{AuthError, AuthSource, auth_store};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Write, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

fn source(mock: &Mock, directory: &crate::auth_private::Directory) -> AuthSource {
    let mut source = mock.source(directory);
    source.config.browser_redirect_uri = "http://127.0.0.1:0/auth/callback".into();
    source
}
fn fields(url: &str) -> BTreeMap<String, String> {
    reqwest::Url::parse(url).unwrap().query_pairs().into_owned().collect()
}
#[test]
fn browser_constructor_rejects_unservable_callback_paths_before_storage() {
    let directory = directory();
    let file = directory.0.join("unopened.json");
    let endpoint = "http://127.0.0.1/unused";
    let mut config = echo_config::ChatGptAuthConfig {
        issuer: endpoint.into(),
        client_id: "mock-client".into(),
        scopes: "mock scopes".into(),
        usercode_endpoint: endpoint.into(),
        device_token_endpoint: endpoint.into(),
        token_endpoint: endpoint.into(),
        authorization_endpoint: endpoint.into(),
        verification_url: endpoint.into(),
        device_redirect_uri: endpoint.into(),
        browser_redirect_uri: String::new(),
    };
    for host in ["localhost", "127.0.0.1"] {
        for path in ["prefix/auth/callback", "/auth/callback", "auth/callback/"] {
            config.browser_redirect_uri = format!("http://{host}:0/{path}"); // lint-rules: allow-literal inert rejected callback configuration, never requested
            assert_eq!(
                AuthSource::new(config.clone(), file.clone()).err(),
                Some(AuthError::InvalidConfig)
            );
            assert!(!file.exists());
        }
        config.browser_redirect_uri = format!("http://{host}:0/auth/callback"); // lint-rules: allow-literal inert loopback constructor configuration, no I/O
        assert_eq!(AuthSource::new(config.clone(), file.clone()).err(), None);
        assert!(!file.exists());
    }
}
async fn send(redirect: &str, query: &str) -> u16 {
    let mut url = reqwest::Url::parse(redirect).unwrap();
    url.set_query(Some(query));
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(url)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}
async fn start(
    source: Arc<AuthSource>,
) -> (
    tokio::task::JoinHandle<Result<crate::ChatGptCredential, AuthError>>,
    String,
) {
    let (tx, rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        source
            .browser_sign_in(|url| {
                tx.send(url).unwrap();
            })
            .await
    });
    (task, rx.await.unwrap())
}
fn assert_query(url: &str, query: &BTreeMap<String, String>) {
    assert_eq!(reqwest::Url::parse(url).unwrap().path(), "/authorize");
    assert_eq!(query.len(), 10);
    for (key, value) in [
        ("response_type", "code"),
        ("client_id", "mock-client"),
        ("scope", "mock scopes"),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", "echo-bridge"),
    ] {
        assert_eq!(query[key], value);
    }
    assert_eq!(query["state"].len(), 43);
}
#[tokio::test]
async fn t3_browser_exact_query_pkce_form_and_reload() {
    let mock = Mock::start(vec![(200, tokens(Some("synthetic-account")))]).await;
    let directory = directory();
    let source = Arc::new(source(&mock, &directory));
    let (task, url) = start(source.clone()).await;
    let query = fields(&url);
    assert_query(&url, &query);
    assert_eq!(
        send(&query["redirect_uri"].replace("/auth/callback", "/wrong"), "").await,
        404
    );
    assert_eq!(mock.requests.lock().unwrap().len(), 0);
    assert_eq!(
        send(
            &query["redirect_uri"],
            &format!("code=synthetic-code&state={}", query["state"])
        )
        .await,
        200
    );
    let credential = task.await.unwrap().unwrap();
    assert_eq!(credential.access_token.expose(), &token(Some("synthetic-account")));
    assert_eq!(source.current().await.unwrap().account_id, "synthetic-account");
    let requests = mock.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.path, "/token");
    assert_eq!(request.headers["content-type"], "application/x-www-form-urlencoded");
    for absent in ["authorization", "originator", "dpop", "user-agent"] {
        assert!(!request.headers.contains_key(absent));
    }
    let form = fields(&format!("http://127.0.0.1/?{}", request.body));
    assert_eq!(form.len(), 5);
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["client_id"], "mock-client");
    assert_eq!(form["code"], "synthetic-code");
    assert_eq!(form["redirect_uri"], query["redirect_uri"]);
    assert_eq!(form["code_verifier"].len(), 43);
    assert_ne!(form["code_verifier"], query["state"]);
    assert_eq!(
        URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes())),
        query["code_challenge"]
    );
    let stored = auth_store::load(&source.file).unwrap();
    assert_eq!(stored.refresh.expose(), "synthetic-refresh");
    writeln!(
        std::io::stdout(),
        "browser exact query keys={:?}; form keys={:?}; S256 matches; exchange count=1",
        query.keys(),
        form.keys()
    )
    .unwrap();
    let url = reqwest::Url::parse(&query["redirect_uri"]).unwrap();
    let rebound = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, url.port().unwrap()))
        .await
        .unwrap();
    drop(rebound);
    mock.stop().await;
}
#[tokio::test]
async fn t3_browser_invalid_callbacks_preserve_old_bytes_without_exchange() {
    for (suffix, expected) in [
        ("code=synthetic-code&state=wrong", AuthError::StateMismatch),
        ("code=a&code=b&state=STATE", AuthError::InvalidCredential),
        ("code=a&state=STATE&state=STATE", AuthError::InvalidCredential),
        ("error=access_denied&state=STATE", AuthError::AuthorizationDenied),
        ("state=STATE", AuthError::InvalidCredential),
        ("code=&state=STATE", AuthError::InvalidCredential),
    ] {
        let mock = Mock::start(vec![]).await;
        let directory = directory();
        let source = Arc::new(source(&mock, &directory));
        auth_store::save(&source.file, &record()).unwrap();
        let old = std::fs::read(&source.file).unwrap();
        let (task, url) = start(source.clone()).await;
        let query = fields(&url);
        assert_eq!(
            send(&query["redirect_uri"], &suffix.replace("STATE", &query["state"])).await,
            400
        );
        assert_eq!(task.await.unwrap().err(), Some(expected));
        assert_eq!(std::fs::read(&source.file).unwrap(), old);
        assert_eq!(mock.requests.lock().unwrap().len(), 0);
        mock.stop().await;
    }
}
#[tokio::test(start_paused = true)]
async fn t3_browser_timeout_and_cancellation_release_listener() {
    for cancel in [false, true] {
        let mock = Mock::start(vec![]).await;
        let directory = directory();
        let source = Arc::new(source(&mock, &directory));
        let (task, url) = start(source.clone()).await;
        let query = fields(&url);
        let redirect = reqwest::Url::parse(&query["redirect_uri"]).unwrap();
        if cancel {
            task.abort();
            assert!(task.await.err().unwrap().is_cancelled());
        } else {
            tokio::time::advance(Duration::from_mins(15)).await;
            assert_eq!(task.await.unwrap().err(), Some(AuthError::Timeout));
        }
        let rebound = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, redirect.port().unwrap()))
            .await
            .unwrap();
        drop(rebound);
        assert_eq!(mock.requests.lock().unwrap().len(), 0);
        assert!(!source.file.exists());
        mock.stop().await;
    }
}
#[tokio::test]
async fn t3_browser_occupied_port_and_unsafe_redirect_refused() {
    let mock = Mock::start(vec![]).await;
    let directory = directory();
    let mut source = source(&mock, &directory);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    source.config.browser_redirect_uri = format!(
        "http://127.0.0.1:{}/auth/callback", // lint-rules: allow-literal reserved loopback callback port
        listener.local_addr().unwrap().port()
    );
    assert_eq!(
        source
            .browser_sign_in(|_| assert_eq!(1, 0, "occupied port callback"))
            .await
            .err(),
        Some(AuthError::Busy)
    );
    for host in ["0.0.0.0", "192.0.2.1", "[::1]"] {
        let mut config = source.config.clone();
        config.browser_redirect_uri = format!("http://{host}:1455/auth/callback"); // lint-rules: allow-literal rejected redirect configuration, never requested
        assert_eq!(
            AuthSource::new(config, source.file.clone()).err(),
            Some(AuthError::InvalidConfig)
        );
    }
    assert_eq!(mock.requests.lock().unwrap().len(), 0);
    mock.stop().await;
}
#[tokio::test]
async fn t3_browser_malformed_and_oversized_requests_return_400() {
    for request in [
        "POST /auth/callback HTTP/1.1\r\n\r\n".to_owned(),
        "GET //bad HTTP/1.1\r\n\r\n".into(),
        "x".repeat(8192),
    ] {
        let mock = Mock::start(vec![]).await;
        let directory = directory();
        let source = Arc::new(source(&mock, &directory));
        let (task, url) = start(source).await;
        let query = fields(&url);
        let redirect = reqwest::Url::parse(&query["redirect_uri"]).unwrap();
        let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, redirect.port().unwrap()))
            .await
            .unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        assert!(response.starts_with(b"HTTP/1.1 400 "));
        assert_eq!(task.await.unwrap().err(), Some(AuthError::InvalidCredential));
        assert_eq!(mock.requests.lock().unwrap().len(), 0);
        mock.stop().await;
    }
}

#[tokio::test]
async fn t3_browser_exchange_failures_do_not_replace_credential() {
    for (status, body, expected) in [
        (400, "denied", AuthError::RelinkRequired),
        (200, "{}", AuthError::InvalidCredential),
    ] {
        let mock = Mock::start(vec![(status, body.into())]).await;
        let directory = directory();
        let source = Arc::new(source(&mock, &directory));
        auth_store::save(&source.file, &record()).unwrap();
        let old = std::fs::read(&source.file).unwrap();
        let (task, url) = start(source.clone()).await;
        let query = fields(&url);
        assert_eq!(
            send(
                &query["redirect_uri"],
                &format!("code=synthetic-code&state={}", query["state"])
            )
            .await,
            200
        );
        assert_eq!(task.await.unwrap().err(), Some(expected));
        assert_eq!(std::fs::read(&source.file).unwrap(), old);
        assert_eq!(mock.requests.lock().unwrap().len(), 1);
        mock.stop().await;
    }
}

#[tokio::test]
async fn t3_browser_stalled_exchange_timeout_or_drop_stops_publication() {
    for cancel in [false, true] {
        let mut mock = Mock::start(vec![(0, String::new())]).await;
        let directory = directory();
        let source = Arc::new(source(&mock, &directory));
        let (task, url) = start(source.clone()).await;
        let query = fields(&url);
        assert_eq!(
            send(
                &query["redirect_uri"],
                &format!("code=synthetic-code&state={}", query["state"])
            )
            .await,
            200
        );
        assert_eq!(mock.seen.recv().await, Some(0));
        if cancel {
            task.abort();
            assert!(task.await.err().unwrap().is_cancelled());
        } else {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(30)).await;
            assert_eq!(task.await.unwrap().err(), Some(AuthError::Timeout));
            tokio::time::resume();
        }
        let url = reqwest::Url::parse(&query["redirect_uri"]).unwrap();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, url.port().unwrap()))
            .await
            .unwrap();
        drop(listener);
        assert!(!source.file.exists());
        assert_eq!(mock.requests.lock().unwrap().len(), 1);
        mock.stop().await;
    }
}
