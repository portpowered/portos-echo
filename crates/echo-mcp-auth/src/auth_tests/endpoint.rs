use super::{
    registration::{EPOCH, REGISTER, approve, offer_registration},
    *,
};
use crate::EndpointTokenSource;
use serde_json::{Value, json};

/// The endpoint scopes plus one read scope the bridge's MCP tools use.
pub(super) const SCOPES: [&str; 4] = ["group:read", "message:send", "endpoint:write", "endpoint:read"];

async fn client(mock: &Mock, directory: &std::path::Path, scopes: &[&str], epoch: u64) -> Arc<AuthClient> {
    let explicit = options(mock, directory);
    let options = AuthOptions {
        resource_url: explicit.resource_url,
        client_id: LinkClient::Automatic {
            name: "Test Echo".into(),
        },
        scopes: scopes.iter().map(|scope| (*scope).into()).collect(),
        storage_dir: explicit.storage_dir,
        request_timeout: explicit.request_timeout,
    };
    Arc::new(
        drive(AuthClient::open_configured(options, Some(epoch), None))
            .await
            .unwrap(),
    )
}

pub(super) async fn linked(mock: &Mock, directory: &std::path::Path, scopes: &[&str]) -> Arc<AuthClient> {
    offer_registration(mock);
    let owner = client(mock, directory, scopes, EPOCH).await;
    approve(mock, owner.clone(), &directory.join("grant.json")).await;
    owner
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_grant_reuses_one_dcr_approval_and_keeps_the_audiences_separate() {
    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    mock.enable_endpoint_audience().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mcp = linked(&mock, home.path(), &SCOPES).await;
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
async fn t3_endpoint_sign_in_names_both_audiences_once_and_keeps_them_with_the_grant() {
    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    mock.enable_endpoint_audience().unwrap();
    let home = tempfile::tempdir().unwrap();
    let owner = linked(&mock, home.path(), &SCOPES).await;
    let mcp = format!("{}/mcp", mock.base);
    {
        let state = mock.state.lock().unwrap();
        let link = state.requests.iter().find(|r| r.path == "/custom/link").unwrap();
        let resources: Vec<_> = link.form_all.iter().filter(|(k, _)| k == "resource").collect();
        assert_eq!(
            resources,
            [
                &("resource".into(), mock.base.clone()),
                &("resource".into(), mcp.clone())
            ]
        );
        assert_eq!(link.form["scope"], SCOPES.join(" "));
        assert_eq!(
            state.endpoint.registered_scope.as_deref(),
            Some(SCOPES.join(" ").as_str())
        );
        let exchange = state.requests.iter().find(|r| r.path == "/custom/token").unwrap();
        assert_eq!(exchange.form["resource"], mcp);
    }
    let record: Value = serde_json::from_slice(&std::fs::read(home.path().join("grant.json")).unwrap()).unwrap();
    assert_eq!(record["grant"]["resources"], json!([mock.base, mcp]));
    assert_eq!(record["grant"]["scope"], SCOPES.join(" "));
    assert_eq!(drive(owner.endpoint_unready(&mock.base)).await, None);
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_grant_from_before_two_audience_sign_in_needs_a_relink_and_is_never_refreshed_for_it() {
    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    mock.enable_endpoint_audience().unwrap();
    let home = tempfile::tempdir().unwrap();
    drop(linked(&mock, home.path(), &SCOPES).await);
    // A grant committed by an earlier build names no resources.
    let path = home.path().join("grant.json");
    let mut record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["grant"].as_object_mut().unwrap().remove("resources");
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();

    let owner = client(&mock, home.path(), &SCOPES, EPOCH + 5).await;
    assert!(owner.has_grant());
    assert_eq!(
        drive(owner.endpoint_unready(&mock.base)).await,
        Some("audience_missing")
    );
    let endpoint = owner.endpoint_grant(&mock.base).unwrap();
    error(drive(endpoint.access_token()).await, AuthError::RelinkRequired);
    error(drive(endpoint.refresh_installation()).await, AuthError::RelinkRequired);
    assert_eq!(
        mock.refreshes(),
        0,
        "a legacy grant is never spent on the REST audience"
    );
    assert_eq!(drive(owner.access_token()).await.unwrap().expose(), "access-1");

    approve(&mock, owner.clone(), &path).await;
    assert_eq!(drive(owner.endpoint_unready(&mock.base)).await, None);
    assert_eq!(
        mock.count(REGISTER),
        1,
        "the registered client already covers the scopes"
    );
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_scopes_beyond_the_registered_client_register_a_new_one() {
    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    mock.enable_endpoint_audience().unwrap();
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("grant.json");
    // An install registered with a narrower scope set before this change.
    let narrow = linked(&mock, home.path(), &["endpoint:read", "group:read"]).await;
    assert_eq!(drive(narrow.endpoint_unready(&mock.base)).await, Some("scopes_missing"));
    error(
        drive(narrow.endpoint_grant(&mock.base).unwrap().access_token()).await,
        AuthError::RelinkRequired,
    );
    drop(narrow);

    // Port OS refuses a request beyond what the client registered.
    let mut record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let stored = record.clone();
    record["registration"]["scope"] = json!(SCOPES.join(" "));
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    let forged = client(&mock, home.path(), &SCOPES, EPOCH + 5).await;
    assert!(drive(forged.begin_link()).await.is_err());
    let refused = forged.take_link_failure().unwrap();
    assert_eq!(
        (refused.step, refused.oauth_error.as_deref()),
        ("device_authorization", Some("invalid_scope"))
    );
    assert_eq!(mock.count(REGISTER), 1);
    drop(forged);

    // The stored registration does not cover the union: the next link registers again.
    std::fs::write(&path, serde_json::to_vec(&stored).unwrap()).unwrap();
    let wider = client(&mock, home.path(), &SCOPES, EPOCH + 5).await;
    approve(&mock, wider.clone(), &path).await;
    assert_eq!(mock.count(REGISTER), 2);
    assert_eq!(stored_scope(&path), SCOPES.join(" "));
    assert!(drive(wider.endpoint_ready(&mock.base)).await);
}

fn stored_scope(path: &std::path::Path) -> String {
    let record: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    record["registration"]["scope"].as_str().unwrap().into()
}

#[tokio::test(start_paused = true)]
async fn t3_portos_mcp_admits_by_audience_then_each_tool_by_its_scope() {
    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    mock.enable_endpoint_audience().unwrap();
    let home = tempfile::tempdir().unwrap();
    let owner = linked(&mock, home.path(), &SCOPES).await;
    let mcp = drive(owner.access_token()).await.unwrap();
    let rest = drive(owner.endpoint_grant(&mock.base).unwrap().access_token())
        .await
        .unwrap();
    for (body, status) in [
        (json!({"method":"initialize"}), 200),
        (json!({"method":"tools/list"}), 200),
        (json!({"method":"tools/call","params":{"name":"group_query"}}), 200),
        (json!({"method":"tools/call","params":{"name":"control"}}), 200),
        (json!({"method":"tools/call","params":{"name":"endpoint_manage"}}), 200),
        (json!({"method":"tools/call","params":{"name":"group_manage"}}), 403),
        (
            json!({"method":"tools/call","params":{"name":"subscription_list"}}),
            403,
        ),
        (json!({"method":"events/subscribe"}), 403),
    ] {
        let (code, challenge) = drive(call(&mock, "/mcp", mcp.expose(), &body)).await;
        assert_eq!(code, status, "{body}");
        assert_eq!(
            challenge.is_some_and(|value| value.contains("insufficient_scope")),
            status == 403
        );
    }
    let (code, _) = drive(call(&mock, "/mcp", rest.expose(), &json!({"method":"initialize"}))).await;
    assert_eq!(code, 401, "a REST-audience token is not admitted to /mcp");
}

#[tokio::test(start_paused = true)]
async fn t3_portos_rest_routes_need_their_scope_and_the_rest_audience() {
    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    mock.enable_endpoint_audience().unwrap();
    let home = tempfile::tempdir().unwrap();
    let owner = linked(&mock, home.path(), &SCOPES).await;
    let mcp = drive(owner.access_token()).await.unwrap();
    let rest = drive(owner.endpoint_grant(&mock.base).unwrap().access_token())
        .await
        .unwrap();
    let (code, _) = drive(call(&mock, "/messages", mcp.expose(), &json!({}))).await;
    assert_eq!(code, 401, "an MCP-audience token is not a REST token");
    mock.state.lock().unwrap().endpoint.scope = "endpoint:read endpoint:write".into();
    let (code, challenge) = drive(call(&mock, "/messages", rest.expose(), &json!({}))).await;
    assert_eq!(code, 403);
    assert!(challenge.unwrap().contains("insufficient_scope"));
}

async fn call(mock: &Mock, path: &str, bearer: &str, body: &Value) -> (u16, Option<String>) {
    let response = reqwest::Client::new()
        .post(format!("{}{path}", mock.base))
        .bearer_auth(bearer)
        .header("Idempotency-Key", "synthetic-key")
        .json(body)
        .send()
        .await
        .unwrap();
    let challenge = response
        .headers()
        .get("www-authenticate")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    (response.status().as_u16(), challenge)
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_grant_serializes_both_audiences_and_never_replays_after_restart() {
    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    mock.enable_endpoint_audience().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mcp = linked(&mock, home.path(), &SCOPES).await;
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
    assert_eq!(durable["grant"]["resources"], json!([mock.base, mcp_resource]));
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
    let reopened = client(&mock, home.path(), &SCOPES, EPOCH).await;
    let endpoint = reopened.endpoint_grant(&mock.base).unwrap();
    error(drive(endpoint.access_token()).await, AuthError::RelinkRequired);
    error(drive(reopened.access_token()).await, AuthError::RelinkRequired);
    assert_eq!(mock.refreshes(), 4);
    assert!(!reopened.has_grant());
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_fresh_token_rotates_the_rest_audience_and_keeps_the_newest_refresh() {
    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    mock.enable_endpoint_audience().unwrap();
    let home = tempfile::tempdir().unwrap();
    let owner = linked(&mock, home.path(), &SCOPES).await;
    let endpoint = owner.endpoint_grant(&mock.base).unwrap();
    assert_eq!(
        drive(endpoint.access_token()).await.unwrap().expose(),
        "endpoint-access-2"
    );
    assert_eq!(
        drive(endpoint.fresh_token()).await.unwrap().expose(),
        "endpoint-access-3"
    );
    let durable: Value = serde_json::from_slice(&std::fs::read(home.path().join("grant.json")).unwrap()).unwrap();
    assert_eq!(durable["grant"]["refresh_token"], "refresh-3");
    assert_refreshes(&mock, &[(&mock.base, "refresh-1"), (&mock.base, "refresh-2")]);
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

#[tokio::test(start_paused = true)]
async fn t3_reports_with_a_request_id_answer_only_open_commands() {
    let mock = Mock::start().await;
    mock.set_epoch(EPOCH);
    mock.enable_endpoint_audience().unwrap();
    let home = tempfile::tempdir().unwrap();
    let owner = linked(&mock, home.path(), &SCOPES).await;
    let rest = drive(owner.endpoint_grant(&mock.base).unwrap().access_token())
        .await
        .unwrap();
    {
        let mut state = mock.state.lock().unwrap();
        state.endpoint.device_id = Some("synthetic-echo-device".into());
        state.endpoint.claimed.insert(rest.expose().clone());
        state.endpoint.open_requests.insert("open-command".into());
    }
    let now_ms = i64::try_from(EPOCH).unwrap() * 1000;
    let report = |payload: Value| {
        json!({
            "header":{"namespace":"port1/systems/zero/plugins/echo-show","name":"report-state","version":"1.0"},
            "target":{"type":"ROUTE","value":"synthetic-echo-device"},
            "payload":payload
        })
    };
    let speaker = report(json!({"sampledAtMs":now_ms,"speaker":{"volume":40,"muted":false}}));
    let answered =
        report(json!({"sampledAtMs":now_ms,"speaker":{"volume":40,"muted":false},"requestId":"set-volume-1"}));
    let reply = report(json!({"requestId":"open-command","error":{"code":"NOT_FOUND"}}));
    let post = |body: &Value, key: &'static str| {
        let request = reqwest::Client::new()
            .post(format!("{}/messages", mock.base))
            .bearer_auth(rest.expose())
            .header("Idempotency-Key", key)
            .json(body)
            .send();
        async move { request.await.unwrap().status().as_u16() }
    };
    assert_eq!(
        drive(post(&speaker, "key-1")).await,
        202,
        "plain state needs no request"
    );
    assert_eq!(
        drive(post(&answered, "key-2")).await,
        404,
        "a fire-and-forget command is never answered"
    );
    assert_eq!(drive(post(&reply, "key-3")).await, 202);
    assert_eq!(drive(post(&reply, "key-3")).await, 202, "a retried key is idempotent");
    assert_eq!(drive(post(&reply, "key-4")).await, 404, "one reply per command");
}
