use super::*;
use crate::wire::Pair;
use serde_json::{Value, json};

#[test]
fn invalid_token_pairs_are_never_published() {
    let valid = json!({"access_token":"synthetic-access","refresh_token":"synthetic-refresh","token_type":"Bearer","expires_in":300,"scope":"endpoint:read"});
    for (key, value) in [
        ("token_type", json!("DPoP")),
        ("access_token", json!("")),
        ("refresh_token", json!("")),
        ("expires_in", json!(0)),
    ] {
        let mut pair = valid.clone();
        pair[key] = value;
        error(
            serde_json::from_value::<Pair>(pair).unwrap().grant(0, &[]),
            AuthError::Protocol,
        );
    }
    error(
        serde_json::from_value::<Pair>(valid.clone())
            .unwrap()
            .grant(u64::MAX, &[]),
        AuthError::Protocol,
    );
    let pair = serde_json::from_value::<Pair>(valid).unwrap().grant(7, &[]).unwrap();
    assert_eq!(pair.expose().expires_at_unix, 307);
}

#[tokio::test(start_paused = true)]
async fn t3_invalid_link_fields_and_polling_errors_leave_no_grant() {
    let h = Harness::new().await;
    let valid = json!({"device_code":"synthetic-device","user_code":"TEST-CODE","verification_uri":format!("{}/approve",h.mock.base),"expires_in":1800,"interval":5});
    for (key, value) in [
        ("device_code", json!("")),
        ("user_code", json!("")),
        ("expires_in", json!(0)),
        ("interval", json!(0)),
        ("verification_uri", json!("file:///bad")),
    ] {
        let mut response = valid.clone();
        response[key] = value;
        h.mock.fault("/custom/link", 200, response);
        error(drive(h.client.begin_link()).await, AuthError::Protocol);
    }
    h.mock
        .fault("/custom/link", 400, json!({"error":"authorization_pending"}));
    error(drive(h.client.begin_link()).await, AuthError::Protocol);
    let mut default = valid;
    default.as_object_mut().unwrap().remove("interval");
    h.mock.fault("/custom/link", 200, default);
    assert_eq!(drive(h.client.begin_link()).await.unwrap().interval, 5);
    h.mock.state.lock().unwrap().fault = None;
    for failure in ["expired_token", "invalid_grant"] {
        h.mock.state.lock().unwrap().polling.push_back(failure.into());
        let link = drive(h.client.begin_link()).await.unwrap();
        let task = tokio::spawn({
            let c = h.client.clone();
            async move { c.await_approval(link).await }
        });
        tokio::task::yield_now().await;
        advance(Duration::from_secs(5)).await;
        error(
            drive(task).await.unwrap(),
            if failure == "expired_token" {
                AuthError::Expired
            } else {
                AuthError::RelinkRequired
            },
        );
        assert_eq!(h.record()["grant"], Value::Null);
    }
}

#[tokio::test(start_paused = true)]
async fn t3_transport_errors_and_unexpected_refresh_poll_replies_are_safe() {
    let h = Harness::linked().await;
    for (status, body) in [
        (503, json!({"error":"synthetic-private-body"})),
        (400, json!({"error":"synthetic-private-body"})),
        (400, json!({"unexpected":true})),
    ] {
        h.mock.fault("/custom/link", status, body);
        let expected = if status == 503 {
            AuthError::Transport
        } else {
            AuthError::Protocol
        };
        error(drive(h.client.begin_link()).await, expected);
    }
    h.mock
        .fault("/custom/token", 400, json!({"error":"authorization_pending"}));
    error(drive(h.client.force_refresh()).await, AuthError::Protocol);
    error(drive(h.client.force_refresh()).await, AuthError::RelinkRequired);
    assert_eq!(h.record()["grant"]["access_token"], "access-1");
}

#[tokio::test(start_paused = true)]
async fn t3_empty_options_and_invalid_resource_discovery_stop_before_posts() {
    let mock = Mock::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut empty = options(&mock, dir.path());
    empty.client_id.clear();
    error(drive(AuthClient::open(empty)).await, AuthError::Discovery);
    let mut empty = options(&mock, dir.path());
    empty.request_timeout = Duration::ZERO;
    error(drive(AuthClient::open(empty)).await, AuthError::Discovery);
    for resource in [
        json!({"resource":"https://example.invalid/mcp","authorization_servers":[mock.base]}), // lint-rules: allow-literal discovery mismatch fixture rejected before resource access
        json!({"resource":format!("{}/mcp",mock.base),"authorization_servers":[]}),
    ] {
        mock.fault("/.well-known/oauth-protected-resource", 200, resource);
        error(
            drive(AuthClient::open(options(&mock, dir.path()))).await,
            AuthError::Discovery,
        );
    }
    assert_eq!(mock.count("/custom/link"), 0);
}

#[tokio::test(start_paused = true)]
async fn t3_revoke_before_link_and_repeated_revoke_are_idempotent() {
    let h = Harness::new().await;
    drive(h.client.revoke()).await.unwrap();
    drive(h.client.revoke()).await.unwrap();
    assert_eq!(h.mock.count("/custom/end"), 0);
    error(drive(h.client.force_refresh()).await, AuthError::RelinkRequired);
}
