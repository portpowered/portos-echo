use super::{
    registration::{EPOCH, REGISTER, linked_dcr},
    *,
};

#[tokio::test(start_paused = true)]
async fn t3_endpoint_grant_keeps_dcr_identity_but_uses_an_independent_audience() {
    use crate::EndpointTokenSource;

    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    mock.enable_endpoint_audience().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let mcp_path = directory.path().join("grant.json");
    let mcp = linked_dcr(&mock, directory.path()).await;
    let original = std::fs::read(&mcp_path).unwrap();
    let endpoint = approve_endpoint(&mcp, directory.path()).await;
    drive(endpoint.refresh_installation()).await.unwrap();
    let endpoint_bearer = drive(endpoint.access_token()).await.unwrap();
    let mcp_bearer = drive(mcp.access_token()).await.unwrap();
    assert!(endpoint_bearer.expose() == "endpoint-access-2");
    assert!(mcp_bearer.expose() == "access-1");
    assert_eq!(drive(mock.resource(mcp_bearer.expose())).await, 200);
    assert_eq!(drive(mock.resource(endpoint_bearer.expose())).await, 401);
    assert_eq!(std::fs::read(mcp_path).unwrap(), original);
    assert_unbound_forms(&mock);
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_grant_serializes_rotation_and_never_replays_a_spent_refresh_after_restart() {
    use crate::{EndpointRegistration, EndpointTokenSource};

    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    mock.enable_endpoint_audience().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mcp = linked_dcr(&mock, home.path()).await;
    let endpoint = Arc::new(approve_endpoint(&mcp, home.path()).await);
    advance(Duration::from_secs(3_301)).await;
    let (gateway, api) = drive(async { tokio::join!(endpoint.access_token(), endpoint.access_token()) }).await;
    assert!(gateway.unwrap().expose() == "endpoint-access-2");
    assert!(api.unwrap().expose() == "endpoint-access-2");
    assert_eq!(mock.refreshes(), 1);
    lose_refresh_response(&mock, endpoint).await;
    assert_eq!(mock.refreshes(), 2);

    let store = Arc::new(EndpointRegistration::open(home.path()).unwrap());
    let reopened = drive(mcp.endpoint_grant(store)).await.unwrap();
    error(drive(reopened.access_token()).await, AuthError::RelinkRequired);
    assert_eq!(mock.refreshes(), 2);
    assert!(mcp.has_grant());
}

async fn approve_endpoint(mcp: &AuthClient, home: &std::path::Path) -> crate::EndpointGrant {
    let registration = Arc::new(crate::EndpointRegistration::open(home).unwrap());
    let endpoint = drive(mcp.endpoint_grant(registration)).await.unwrap();
    let challenge = drive(endpoint.begin_approval()).await.unwrap();
    assert_eq!(challenge.user_code(), "TEST-CODE");
    let (approved, ()) =
        drive(async { tokio::join!(endpoint.await_approval(challenge), advance(Duration::from_secs(5))) }).await;
    approved.unwrap();
    endpoint
}

async fn lose_refresh_response(mock: &Mock, endpoint: Arc<crate::EndpointGrant>) {
    mock.state.lock().unwrap().hold = Some("/custom/token".into());
    let refresh = tokio::spawn(async move { endpoint.refresh_installation().await });
    until(|| mock.state.lock().unwrap().held).await;
    refresh.abort();
    assert!(refresh.await.unwrap_err().is_cancelled());
    mock.release.notify_one();
}

fn assert_unbound_forms(mock: &Mock) {
    let state = mock.state.lock().unwrap();
    let expected = [
        ("/custom/link", vec!["client_id", "scope"]),
        ("/custom/token", vec!["client_id", "device_code", "grant_type"]),
        ("/custom/token", vec!["client_id", "grant_type", "refresh_token"]),
    ];
    for (request, (path, fields)) in state.requests.iter().skip(5).take(3).zip(expected) {
        assert_eq!(request.path, path);
        assert_eq!(request.form["client_id"], "dcr_synthetic");
        assert_eq!(request.form.keys().map(String::as_str).collect::<Vec<_>>(), fields);
        assert!(request.headers.contains_key("dpop"));
        assert!(!request.headers.contains_key("authorization"));
    }
    assert_eq!(state.requests.iter().filter(|req| req.path == REGISTER).count(), 1);
    assert_eq!(
        state.requests.iter().filter(|req| req.path == "/custom/token").count(),
        3
    );
}
