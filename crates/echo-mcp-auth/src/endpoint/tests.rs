use super::*;

#[tokio::test]
async fn endpoint_install_resumes_the_committed_exchange_and_keeps_the_read_back_endpoint() {
    let home = tempfile::tempdir().unwrap();
    let first = EndpointRegistration::open(home.path()).unwrap();
    let pending = first.exchange("Hall Echo").await.unwrap().unwrap();
    let key = pending.key().to_owned();
    let body = serde_json::to_value(pending.request()).unwrap();
    let device = body["pluginAuthenticationData"]["deviceId"].as_str().unwrap();
    drop(first);

    let reopened = EndpointRegistration::open(home.path()).unwrap();
    let replay = reopened.exchange("Kitchen").await.unwrap().unwrap();
    assert_eq!(replay.key(), key);
    assert_eq!(serde_json::to_value(replay.request()).unwrap(), body);
    reopened
        .complete_exchange(replay, "synthetic-link".into())
        .await
        .unwrap();
    assert_eq!(reopened.device_id().await, device);
    assert_eq!(
        reopened.remember_endpoint("synthetic-link", " ".into()).await,
        Err(crate::AuthError::Protocol)
    );
    assert_eq!(
        reopened
            .remember_endpoint("another-link", "synthetic-endpoint".into())
            .await,
        Err(crate::AuthError::Protocol)
    );
    reopened
        .remember_endpoint("synthetic-link", "synthetic-endpoint".into())
        .await
        .unwrap();
    let install = reopened.installation().await.unwrap();
    assert_eq!(install.endpoint_id.as_deref(), Some("synthetic-endpoint"));
    let rename = reopened.exchange("Kitchen").await.unwrap().unwrap();
    let body = serde_json::to_value(rename.request()).unwrap();
    assert_eq!(body["authLinkId"], "synthetic-link");
    assert_eq!(body["pluginAuthenticationData"]["deviceId"], device);
    assert_eq!(body["pluginAuthenticationData"]["deviceName"], "Kitchen");
    assert_ne!(rename.key(), key);

    reopened.unlink().await.unwrap();
    assert!(reopened.installation().await.is_none());
    assert_eq!(
        reopened.complete_exchange(rename, "synthetic-link".into()).await,
        Err(crate::AuthError::Protocol)
    );
}
use crate::mock::Mock;
use echo_contracts::secret::Secret;
use serde_json::json;

struct EndpointToken;

impl EndpointTokenSource for EndpointToken {
    fn access_token(&self) -> TokenFuture<'_> {
        Box::pin(async { Ok(Secret::new("synthetic-endpoint-access".into())) })
    }
}

#[test]
fn endpoint_exchange_requires_a_secure_base() {
    let timeout = Duration::from_secs(2);
    for value in [
        "http://127.0.0.1",
        "https://@127.0.0.1", // lint-rules: allow-literal offline credentials rejection fixture
        "https://127.0.0.1/?query",
        "https://127.0.0.1/#fragment",
        "https://127.0.0.1/{substitution}",
    ] {
        assert_eq!(
            EndpointExchange::open(value, timeout, None).err(),
            Some(ExchangeError::Unavailable)
        );
    }
    assert!(EndpointExchange::open("https://127.0.0.1", timeout, None).is_ok());
}

#[tokio::test]
async fn t3_endpoint_exchange_uses_one_path_and_endpoint_bearer() {
    let mock = Mock::start().await;
    let exchange = EndpointExchange::new(Url::parse(&mock.base).unwrap(), Duration::from_secs(2), None).unwrap();
    let device_id = "00000000-0000-4000-8000-000000000001";
    let key = "00000000-0000-4000-8000-000000000002";
    let auth_link = "synthetic/link %";
    let result = json!({"authLinkId": auth_link, "id": "exchange-1", "etag": "v1", "status": "SUCCEEDED"});
    mock.fault(EXCHANGE_PATH, 200, result.clone());
    let install = InstallRequest::new(device_id, "Hall Echo", None);
    let installed = exchange.submit(&EndpointToken, key, &install).await.unwrap();
    assert_eq!(installed.auth_link_id, auth_link);
    assert_eq!(installed.id, "exchange-1");
    assert_eq!(installed.etag, "v1");

    let rename_key = "00000000-0000-4000-8000-000000000003";
    let rename = InstallRequest::new(device_id, "Kitchen Echo", Some(auth_link));
    mock.fault(EXCHANGE_PATH, 200, result);
    assert_eq!(
        exchange
            .submit(&EndpointToken, rename_key, &rename)
            .await
            .unwrap()
            .auth_link_id,
        auth_link
    );
    let duplicate_key = "00000000-0000-4000-8000-000000000004";
    mock.fault(EXCHANGE_PATH, 409, json!({"error": "conflict"}));
    assert_eq!(
        exchange.submit(&EndpointToken, duplicate_key, &install).await.err(),
        Some(ExchangeError::Conflict)
    );

    let state = mock.state.lock().unwrap();
    let expected = |name: &str| {
        json!({
            "principalId": "~self",
            "pluginId": "port1/systems/zero/plugins/15badf8f-fe9b-4bf0-be33-2d7eb287f89f",
            "pluginAuthenticationData": {"deviceId": device_id, "deviceName": name, "model": "Echo Show"}
        })
    };
    let mut renamed = expected("Kitchen Echo");
    renamed["authLinkId"] = json!(auth_link);
    assert_eq!(state.requests.len(), 3);
    for (request, (key, body)) in state.requests.iter().zip([
        (key, expected("Hall Echo")),
        (rename_key, renamed),
        (duplicate_key, expected("Hall Echo")),
    ]) {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, EXCHANGE_PATH);
        assert_eq!(request.headers["authorization"], "Bearer synthetic-endpoint-access");
        assert_eq!(request.headers["idempotency-key"], key);
        assert!(!request.headers.contains_key("dpop"));
        assert_eq!(request.json.as_ref(), Some(&body));
    }
}

#[test]
fn endpoint_exchange_refuses_unusable_configured_roots() {
    let timeout = Duration::from_secs(2);
    for pem in [
        b"".as_slice(),
        b"not PEM",
        b"-----BEGIN CERTIFICATE-----\nbad\n-----END CERTIFICATE-----",
    ] {
        assert_eq!(
            EndpointExchange::open("https://127.0.0.1", timeout, Some(pem)).err(),
            Some(ExchangeError::Unavailable)
        );
    }
}

/// Each failure keeps its HTTP status and a fixed reason; only an unanswered
/// request and a server error are sent again.
#[tokio::test]
async fn t3_endpoint_exchange_failures_keep_status_and_reason() {
    let mock = Mock::start().await;
    let exchange = EndpointExchange::new(Url::parse(&mock.base).unwrap(), Duration::from_secs(2), None).unwrap();
    let install = InstallRequest::new("00000000-0000-4000-8000-000000000001", "Hall Echo", None);
    let key = "00000000-0000-4000-8000-000000000002";
    for (status, expected, reason, retryable) in [
        (503, ExchangeError::Server(503), "server_error", true),
        (401, ExchangeError::Rejected(401), "rejected", false),
        (409, ExchangeError::Conflict, "conflict", false),
    ] {
        mock.fault(EXCHANGE_PATH, status, json!({"error": "synthetic"}));
        let error = exchange.submit(&EndpointToken, key, &install).await.err().unwrap();
        assert_eq!(error, expected);
        assert_eq!(
            (error.reason(), error.http_status(), error.retryable()),
            (reason, Some(status), retryable)
        );
    }
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", closed.local_addr().unwrap()); // lint-rules: allow-literal loopback listener address
    drop(closed);
    let refused = EndpointExchange::new(Url::parse(&base).unwrap(), Duration::from_secs(2), None).unwrap();
    let error = refused.submit(&EndpointToken, key, &install).await.err().unwrap();
    assert_eq!(error, ExchangeError::Transport(Unanswered::Connect));
    assert_eq!(
        (error.reason(), error.http_status(), error.retryable()),
        ("connect", None, true)
    );
}
