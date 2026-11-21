use super::{
    registration::{EPOCH, REGISTER, automatic, linked_dcr},
    *,
};
use crate::EndpointTokenSource;

#[tokio::test(start_paused = true)]
async fn t3_endpoint_grant_reuses_one_dcr_approval_and_keeps_the_audiences_separate() {
    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    mock.enable_endpoint_audience().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mcp = linked_dcr(&mock, home.path()).await;
    let endpoint = mcp.endpoint_grant(&mock.base).unwrap();
    let (gateway, api) = drive(async { tokio::join!(endpoint.access_token(), endpoint.access_token()) }).await;
    assert_eq!(gateway.unwrap().expose(), "endpoint-access-2");
    let api = api.unwrap();
    assert_eq!(api.expose(), "endpoint-access-2");
    let mcp_bearer = drive(mcp.access_token()).await.unwrap();
    assert_eq!(mcp_bearer.expose(), "access-1");
    assert_eq!(drive(mock.resource(mcp_bearer.expose())).await, 200);
    assert_eq!(drive(mock.resource(api.expose())).await, 401);
    assert_eq!(mock.count(REGISTER), 1);
    assert_eq!(mock.count("/custom/link"), 1);
    assert_eq!(mock.count("/custom/token"), 2);
    assert_refreshes(&mock, &[(&mock.base, "refresh-1")]);
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_grant_serializes_both_audiences_and_never_replays_after_restart() {
    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    mock.enable_endpoint_audience().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mcp = linked_dcr(&mock, home.path()).await;
    let endpoint = mcp.endpoint_grant(&mock.base).unwrap();
    assert_eq!(
        drive(endpoint.access_token()).await.unwrap().expose(),
        "endpoint-access-2"
    );
    advance(Duration::from_secs(3_301)).await;
    let (tool, gateway) = drive(async { tokio::join!(mcp.access_token(), endpoint.access_token()) }).await;
    assert_eq!(tool.unwrap().expose(), "access-3");
    assert_eq!(gateway.unwrap().expose(), "endpoint-access-4");
    let mcp_resource = format!("{}/mcp", mock.base);
    let path = home.path().join("grant.json");
    let durable: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(durable["grant"]["refresh_token"], "refresh-4");
    assert_eq!(durable["grant"]["access_token"], "access-3");
    assert_refreshes(
        &mock,
        &[
            (&mock.base, "refresh-1"),
            (&mcp_resource, "refresh-2"),
            (&mock.base, "refresh-3"),
        ],
    );

    advance(Duration::from_secs(3_301)).await;
    lose_refresh_response(&mock, mcp.clone()).await;
    error(drive(mcp.access_token()).await, AuthError::RelinkRequired);
    let reopened = automatic(&mock, home.path(), EPOCH).await;
    let endpoint = reopened.endpoint_grant(&mock.base).unwrap();
    error(drive(endpoint.access_token()).await, AuthError::RelinkRequired);
    error(drive(reopened.access_token()).await, AuthError::RelinkRequired);
    assert_eq!(mock.refreshes(), 4);
    assert!(!reopened.has_grant());
}

async fn lose_refresh_response(mock: &Mock, owner: Arc<AuthClient>) {
    mock.state.lock().unwrap().hold = Some("/custom/token".into());
    let resource = mock.base.clone();
    let refresh = tokio::spawn(async move { owner.endpoint_grant(&resource).unwrap().access_token().await });
    until(|| mock.state.lock().unwrap().held).await;
    refresh.abort();
    assert!(refresh.await.err().unwrap().is_cancelled());
    mock.release.notify_one();
}

fn assert_refreshes(mock: &Mock, expected: &[(&str, &str)]) {
    let state = mock.state.lock().unwrap();
    let refreshes: Vec<_> = state
        .requests
        .iter()
        .filter(|req| req.form.get("grant_type").is_some_and(|grant| grant == "refresh_token"))
        .collect();
    assert_eq!(refreshes.len(), expected.len());
    for (request, (resource, refresh)) in refreshes.into_iter().zip(expected) {
        assert_eq!(request.path, "/custom/token");
        assert_eq!(request.form["client_id"], "dcr_synthetic");
        assert_eq!(request.form["resource"], *resource);
        assert_eq!(request.form["refresh_token"], *refresh);
        assert!(request.headers.contains_key("dpop"));
        assert!(!request.headers.contains_key("authorization"));
    }
}
