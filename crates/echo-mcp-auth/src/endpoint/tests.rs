use super::*;
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
        "https://@api.example.invalid",
        "https://api.example.invalid/?query",
        "https://api.example.invalid/#fragment",
        "https://api.example.invalid/{substitution}",
    ] {
        assert_eq!(
            EndpointExchange::open(value, timeout).err(),
            Some(ExchangeError::Unavailable)
        );
    }
    assert!(EndpointExchange::open("https://api.example.invalid", timeout).is_ok());
}

#[tokio::test]
async fn t3_endpoint_exchange_uses_one_path_and_endpoint_bearer() {
    let mock = Mock::start().await;
    let exchange = EndpointExchange::new(Url::parse(&mock.base).unwrap(), Duration::from_secs(2)).unwrap();
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
            "pluginId": "port1/systems/zero/plugins/echo-show",
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
