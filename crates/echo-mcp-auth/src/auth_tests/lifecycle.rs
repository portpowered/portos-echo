use super::*;
use serde_json::json;

#[tokio::test(start_paused = true)]
async fn t3_discovery_poll_schedule_rotation_and_revoke() {
    let h = Harness::new().await;
    h.mock
        .state
        .lock()
        .unwrap()
        .polling
        .extend(["authorization_pending".into(), "slow_down".into()]);
    let link = drive(h.client.begin_link()).await.unwrap();
    assert_eq!(link.user_code, "TEST-CODE");
    assert_eq!(link.verification_uri, format!("{}/approve", h.mock.base));
    assert_eq!((link.expires_in, link.interval), (1800, 5));
    let task = start_approval(h.client.clone(), link).await;
    for (seconds, count) in [(5, 1), (5, 2), (10, 3)] {
        advance(Duration::from_secs(seconds)).await;
        until(|| h.mock.count("/custom/token") == count).await;
        // The poll holds this lock until its reply is processed.
        drop(drive(h.client.state.lock()).await);
    }
    drive(task).await.unwrap().unwrap();
    assert_eq!(h.mock.count("/.well-known/oauth-protected-resource"), 1);
    assert_eq!(h.mock.count("/.well-known/oauth-authorization-server"), 1);
    assert_link_requests(&h);
    let first = h.record();
    assert_eq!(first["grant"]["scope"], "endpoint:read");
    for generation in 2..=3 {
        let access = drive(h.client.force_refresh()).await.unwrap();
        let record = h.record();
        assert_eq!(record["grant"]["access_token"], *access.expose());
        assert_eq!(record["grant"]["refresh_token"], format!("refresh-{generation}"));
        assert_eq!(record["private_key_jwk"], first["private_key_jwk"]);
    }
    assert_eq!(h.mock.refreshes(), 2);
    assert_eq!(drive(h.mock.resource("access-3")).await, 200);
    drive(h.client.revoke()).await.unwrap();
    assert_eq!(drive(h.mock.resource("access-3")).await, 401);
    assert_eq!(h.record()["grant"], json!(null));
    let reloaded = h.reload().await.unwrap();
    error(drive(reloaded.access_token()).await, AuthError::RelinkRequired);
    assert_eq!(h.record()["private_key_jwk"], first["private_key_jwk"]);
    let state = h.mock.state.lock().unwrap();
    let revocation = state.requests.iter().find(|r| r.path == "/custom/end").unwrap();
    assert_eq!(revocation.form["token"], "refresh-3");
    assert_eq!(revocation.form["client_id"], "synthetic-cli");
    for r in &state.requests {
        assert_eq!(
            r.headers.contains_key("dpop"),
            matches!(r.path.as_str(), "/custom/link" | "/custom/token")
        );
        assert!(!r.headers.contains_key("dpop-nonce"));
        if r.path == "/mcp" {
            assert_eq!(r.headers["authorization"], "Bearer access-3");
        }
    }
    assert_eq!(state.jtis.len(), 6);
}

fn assert_link_requests(h: &Harness) {
    let requests = h.mock.state.lock().unwrap().requests.clone();
    let device = requests.iter().find(|r| r.path == "/custom/link").unwrap();
    assert_eq!(device.form.get("resource"), Some(&format!("{}/mcp", h.mock.base)));
    assert_eq!(device.form.get("client_id").map(String::as_str), Some("synthetic-cli"));
    assert_eq!(device.form.get("scope").map(String::as_str), Some("endpoint:read"));
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.path == "/custom/token")
            .map(|r| r.time)
            .collect::<Vec<_>>(),
        [5, 10, 20]
    );
    for r in requests.iter().filter(|r| r.path == "/custom/token") {
        assert_eq!(r.form["grant_type"], "urn:ietf:params:oauth:grant-type:device_code");
        assert_eq!(r.form["device_code"], "synthetic-device");
        assert_eq!(r.form["client_id"], "synthetic-cli");
    }
}

#[tokio::test(start_paused = true)]
async fn t3_refresh_window_and_single_flight_include_short_lived_tokens() {
    let h = Harness::linked().await;
    advance(Duration::from_secs(3299)).await;
    assert_eq!(drive(h.client.access_token()).await.unwrap().expose(), "access-1");
    assert_eq!(h.mock.refreshes(), 0);
    advance(Duration::from_secs(1)).await;
    h.mock.state.lock().unwrap().lifetime = 300;
    h.mock.state.lock().unwrap().hold = Some("/custom/token".into());
    let first = tokio::spawn({
        let c = h.client.clone();
        async move { c.access_token().await }
    });
    until(|| h.mock.state.lock().unwrap().held).await;
    let second = tokio::spawn({
        let c = h.client.clone();
        async move { c.access_token().await }
    });
    // The waiter has entered access_token and is queued behind the first lock.
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(h.mock.refreshes(), 1);
    h.mock.release.notify_one();
    assert_eq!(drive(first).await.unwrap().unwrap().expose(), "access-2");
    assert_eq!(drive(second).await.unwrap().unwrap().expose(), "access-2");
    assert_eq!(h.mock.refreshes(), 1);
    assert_eq!(h.record()["grant"]["access_token"], "access-2");
}

#[tokio::test(start_paused = true)]
async fn t3_replay_revokes_and_clears_local_grant() {
    let h = Harness::linked().await;
    drive(h.client.force_refresh()).await.unwrap();
    let state = h.client.state.lock().await;
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", "refresh-1"),
        ("client_id", "synthetic-cli"),
    ];
    error(
        drive(crate::wire::post::<serde_json::Value>(
            &h.client.http,
            &h.client.metadata.token_endpoint,
            &form,
            Some(&state.record),
            h.client.now(),
        ))
        .await,
        AuthError::RelinkRequired,
    );
    drop(state);
    assert_eq!(drive(h.mock.resource("access-2")).await, 401);
    error(drive(h.client.force_refresh()).await, AuthError::RelinkRequired);
    assert_eq!(h.record()["grant"], json!(null));
    let count = h.mock.refreshes();
    error(drive(h.client.access_token()).await, AuthError::RelinkRequired);
    assert_eq!(h.mock.refreshes(), count);
}

#[tokio::test(start_paused = true)]
async fn t3_overlapping_refresh_then_revoke_cannot_resurrect_grant() {
    let h = Harness::linked().await;
    h.mock.state.lock().unwrap().hold = Some("/custom/token".into());
    let refresh = tokio::spawn({
        let c = h.client.clone();
        async move { c.force_refresh().await }
    });
    until(|| h.mock.state.lock().unwrap().held).await;
    let revoke = tokio::spawn({
        let c = h.client.clone();
        async move { c.revoke().await }
    });
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(h.mock.count("/custom/end"), 0);
    h.mock.release.notify_one();
    drive(refresh).await.unwrap().unwrap();
    drive(revoke).await.unwrap().unwrap();
    error(drive(h.client.access_token()).await, AuthError::RelinkRequired);
    assert_eq!(h.record()["grant"], json!(null));
    assert_eq!(drive(h.mock.resource("access-2")).await, 401);
}

#[tokio::test(start_paused = true)]
async fn t3_explicit_epoch_preserves_elapsed_clock_and_normal_open_wall_sample() {
    let mock = Mock::start().await;
    let directory = tempfile::tempdir().unwrap();
    let epoch = 1_800_000_000;
    let client = drive(AuthClient::open_at_epoch(options(&mock, directory.path()), epoch))
        .await
        .unwrap();
    assert_eq!(client.now(), epoch);
    advance(Duration::from_secs(37)).await;
    assert_eq!(client.now(), epoch + 37);
    // Normal open must still sample wall UTC after discovery/storage, regardless
    // of an earlier explicit epoch or this runtime's already-advanced clock.
    let normal_dir = tempfile::tempdir().unwrap();
    let before = wire::unix_now().unwrap();
    let normal = drive(AuthClient::open(options(&mock, normal_dir.path())))
        .await
        .unwrap();
    let after = wire::unix_now().unwrap();
    assert!((before..=after).contains(&normal.now()));
    advance(Duration::from_secs(5)).await;
    assert_eq!(client.now(), epoch + 42);
    assert_eq!(normal.now(), normal.epoch + 5);
}
