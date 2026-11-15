use super::*;
use crate::{proof, wire};
use serde_json::{Value, json};

#[tokio::test(start_paused = true)]
async fn t3_wrong_resource_missing_resource_and_wrong_proof_endpoint() {
    let h = Harness::new().await;
    let state = h.client.state.lock().await;
    let url = &h.client.metadata.device_authorization_endpoint;
    let key = state.record.private_key_jwk.expose();
    let wrong = proof::sign(key, &h.client.metadata.token_endpoint, h.client.now()).unwrap();
    let response = drive(
        h.client
            .http
            .post(url)
            .header("DPoP", wrong.expose())
            .form(&[("resource", format!("{}/mcp", h.mock.base))])
            .send(),
    )
    .await
    .unwrap();
    assert_eq!(response.status().as_u16(), 400);
    let body: Value = drive(response.json()).await.unwrap();
    assert_eq!(body, json!({"error":"invalid_dpop_proof"}));
    assert_wrong_resource(&h, &state).await;
    let missing = drive(wire::post::<Value>(
        &h.client.http,
        url,
        &[("client_id", "synthetic-cli")],
        Some(&state.record),
        h.client.now(),
    ))
    .await
    .unwrap();
    assert!(matches!(missing, wire::Reply::Value(_)));
    let form = [
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ("device_code", "synthetic-device"),
    ];
    let wire::Reply::Value(pair) = drive(wire::post::<Value>(
        &h.client.http,
        &h.client.metadata.token_endpoint,
        &form,
        Some(&state.record),
        h.client.now(),
    ))
    .await
    .unwrap() else {
        unreachable!()
    };
    assert_eq!(
        drive(h.mock.resource(pair["access_token"].as_str().unwrap())).await,
        401
    );
}

async fn assert_wrong_resource(h: &Harness, state: &State) {
    let url = &h.client.metadata.device_authorization_endpoint;
    let key = state.record.private_key_jwk.expose();
    error(
        drive(wire::post::<Value>(
            &h.client.http,
            url,
            &[("resource", "wrong")],
            Some(&state.record),
            h.client.now(),
        ))
        .await,
        AuthError::Protocol,
    );
    // Assert the exact wire error independently of the client's safe category.
    let signed = proof::sign(key, url, h.client.now()).unwrap();
    let reply = drive(
        h.client
            .http
            .post(url)
            .header("DPoP", signed.expose())
            .form(&[("resource", "wrong")])
            .send(),
    )
    .await
    .unwrap();
    assert_eq!(reply.status().as_u16(), 400);
    assert_eq!(
        drive(reply.json::<Value>()).await.unwrap(),
        json!({"error":"invalid_target"})
    );
}

#[tokio::test(start_paused = true)]
async fn t3_denial_expiry_and_cancellation_stop_polling() {
    let h = Harness::new().await;
    h.mock.state.lock().unwrap().polling.push_back("access_denied".into());
    let link = drive(h.client.begin_link()).await.unwrap();
    let task = tokio::spawn({
        let c = h.client.clone();
        async move { c.await_approval(link).await }
    });
    tokio::task::yield_now().await;
    advance(Duration::from_secs(5)).await;
    error(drive(task).await.unwrap(), AuthError::AccessDenied);
    let mut link = drive(h.client.begin_link()).await.unwrap();
    link.expires_in = 5;
    let task = tokio::spawn({
        let c = h.client.clone();
        async move { c.await_approval(link).await }
    });
    tokio::task::yield_now().await;
    advance(Duration::from_secs(5)).await;
    error(drive(task).await.unwrap(), AuthError::Expired);
    let link = drive(h.client.begin_link()).await.unwrap();
    let task = tokio::spawn({
        let c = h.client.clone();
        async move { c.await_approval(link).await }
    });
    tokio::task::yield_now().await;
    task.abort();
    assert!(drive(task).await.unwrap_err().is_cancelled());
    let count = h.mock.count("/custom/token");
    advance(Duration::from_secs(2000)).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(h.mock.count("/custom/token"), count);
    assert_eq!(count, 1);
    assert_eq!(h.record()["grant"], json!(null));
}

#[tokio::test(start_paused = true)]
async fn t3_faults_are_bounded_redacted_and_revoke_can_be_retried() {
    let h = Harness::linked().await;
    let sentinel = "synthetic-private-body-sentinel";
    h.mock.fault("/custom/end", 503, json!({"error":sentinel}));
    let failure = drive(h.client.revoke()).await.err().unwrap();
    assert_eq!(failure, AuthError::Transport);
    assert!(!format!("{failure:?} {failure}").contains(sentinel));
    assert_eq!(h.record()["grant"]["refresh_token"], "refresh-1");
    error(drive(h.client.access_token()).await, AuthError::RelinkRequired);
    h.mock.state.lock().unwrap().fault = None;
    drive(h.client.revoke()).await.unwrap();
    assert_eq!(h.mock.count("/custom/end"), 2);
    assert_eq!(h.record()["grant"], json!(null));
}

#[tokio::test(start_paused = true)]
async fn t3_stalled_refresh_times_out_without_hidden_replay() {
    let h = Harness::linked().await;
    h.mock.state.lock().unwrap().hold = Some("/custom/token".into());
    let task = tokio::spawn({
        let c = h.client.clone();
        async move { c.force_refresh().await }
    });
    until(|| h.mock.state.lock().unwrap().held).await;
    advance(Duration::from_secs(3)).await;
    error(drive(task).await.unwrap(), AuthError::Transport);
    error(drive(h.client.force_refresh()).await, AuthError::RelinkRequired);
    assert_eq!(h.mock.refreshes(), 1);
    assert_eq!(h.record()["grant"]["access_token"], "access-1");
}

#[tokio::test(start_paused = true)]
async fn t3_discovery_rejects_missing_unsupported_unsafe_or_redirected_metadata() {
    let mock = Mock::start().await;
    let dir = tempfile::tempdir().unwrap();
    for change in ["missing", "algorithm", "unsafe", "cross_origin"] {
        let mut metadata = mock::metadata(&mock.base);
        match change {
            "missing" => {
                metadata.as_object_mut().unwrap().remove("token_endpoint");
            }
            "algorithm" => metadata["dpop_signing_alg_values_supported"] = json!(["RS256"]),
            "unsafe" => metadata["token_endpoint"] = json!("http://example.invalid/token"),
            _ => metadata["token_endpoint"] = json!("https://example.invalid/token"),
        }
        mock.state.lock().unwrap().metadata = Some(metadata);
        error(
            drive(AuthClient::open(options(&mock, dir.path()))).await,
            AuthError::Discovery,
        );
    }
    mock.fault("/.well-known/oauth-protected-resource", 302, json!({}));
    error(
        drive(AuthClient::open(options(&mock, dir.path()))).await,
        AuthError::Transport,
    );
    assert_eq!(mock.count("/custom/link"), 0);
    assert!(!dir.path().join("grant.json").exists());
}

#[test]
fn unsafe_urls_and_safe_error_categories() {
    for bad in [
        "bad",
        "file:///tmp/grant",
        "http://example.invalid",
        "https://user:password@example.invalid",
        "https://example.invalid/#fragment",
    ] {
        error(crate::discovery::safe_url(bad), AuthError::Discovery);
    }
    for good in ["https://example.invalid/token", "http://127.0.0.1:1234/custom"] {
        assert_eq!(crate::discovery::safe_url(good).unwrap().as_str(), good);
    }
    for failure in [
        AuthError::Discovery,
        AuthError::Transport,
        AuthError::Protocol,
        AuthError::AccessDenied,
        AuthError::Expired,
        AuthError::RelinkRequired,
        AuthError::Storage,
    ] {
        assert!(!failure.to_string().contains("synthetic-private"));
    }
}
