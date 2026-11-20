//! T3: grant scopes survive the actual link, rotation and private-store reload.
use super::*;

#[tokio::test(start_paused = true)]
async fn t3_portos_full_and_narrow_grants_preserve_scopes_across_rotation_and_reload() {
    for scopes in [
        echo_config::McpConfig::default().scopes,
        vec!["endpoint:read".into()],
        vec!["subscription:read".into(), "subscription:write".into()],
    ] {
        let h = Harness::with_scopes(scopes.clone()).await;
        h.link().await;
        assert_eq!(h.fixture.shared.lock().unwrap().scopes, scopes.join(" "));
        for refresh in [false, true] {
            if refresh {
                drive(h.client.force_refresh()).await.unwrap();
            }
            let token = drive(h.client.access_token()).await.unwrap();
            let (_, claims, _, _) = mcp_proof::decode(token.expose()).unwrap();
            assert_eq!(claims["scope"], scopes.join(" "));
            assert_eq!(h.request(token.expose(), "initialize").await.status(), 200);
            let other_resource = drive(
                http()
                    .post(format!("{}/mcp", h.fixture.base()))
                    .bearer_auth(token.expose())
                    .json(&json!({"method":"tools/call","params":control()}))
                    .send(),
            )
            .await
            .unwrap();
            assert_eq!(other_resource.status(), 401);
            assert_eq!(h.record()["grant"]["scope"], scopes.join(" "));
        }
        let mut options = h.options();
        options.scopes.clone_from(&scopes);
        let reloaded = drive(AuthClient::open_at_epoch(options, 1_800_000_005)).await.unwrap();
        assert_eq!(reloaded.granted_scopes().await, Some(scopes.clone()));
        let token = drive(reloaded.access_token()).await.unwrap();
        let (_, claims, _, _) = mcp_proof::decode(token.expose()).unwrap();
        assert_eq!(claims["scope"], scopes.join(" "));
        assert_eq!(h.request(token.expose(), "initialize").await.status(), 200);
        assert_eq!(h.fixture.counts().unwrap().refreshes, 1);
        drop(reloaded);
        h.close().await;
    }
}

struct HomeHarness {
    fixture: Fixture,
    client: Arc<AuthClient>,
    _dir: tempfile::TempDir,
}

impl HomeHarness {
    async fn linked(scopes: Vec<String>) -> Self {
        let fixture = Fixture::bind(1_800_000_000).await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let options = AuthOptions {
            resource_url: format!("{}/mcp", fixture.base()),
            client_id: "echo-lab".into(),
            scopes,
            storage_dir: dir.path().into(),
            request_timeout: Duration::from_secs(2),
        };
        let client = Arc::new(drive(AuthClient::open_at_epoch(options, 1_800_000_000)).await.unwrap());
        let code = drive(client.begin_link()).await.unwrap();
        fixture.approve(Approval::Approved).unwrap();
        let polling = client.clone();
        let (task, registered) = approval_task(
            async move { polling.await_approval(code).await },
            std::future::ready(()),
        );
        drive(registered).await.unwrap();
        fixture.advance(5).unwrap();
        tokio::time::advance(Duration::from_secs(5)).await;
        drive(task).await.unwrap().unwrap();
        Self {
            fixture,
            client,
            _dir: dir,
        }
    }

    async fn request(&self, method: &str, params: Value) -> reqwest::Response {
        let access = drive(self.client.access_token()).await.unwrap();
        drive(
            http()
                .post(format!("{}/mcp", self.fixture.base()))
                .bearer_auth(access.expose())
                .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
                .send(),
        )
        .await
        .unwrap()
    }
}

fn control() -> Value {
    json!({"name":"control","arguments":{
        "id":"synthetic/endpoint-groups/twin-room","message":"power/messages/turn-off","payload":{}
    }})
}

#[tokio::test(start_paused = true)]
async fn t3_portos_read_only_control_is_denied_before_any_home_effect() {
    let h = HomeHarness::linked(vec!["endpoint:read".into()]).await;
    let read = h
        .request("tools/call", json!({"name":"endpoint_query","arguments":{}}))
        .await;
    assert_eq!(read.status(), 200);
    let read: Value = drive(read.json()).await.unwrap();
    assert_eq!(read["result"]["isError"], false);
    let denied = h.request("tools/call", control()).await;
    assert_eq!(denied.status(), 403);
    assert_eq!(
        denied.headers()["www-authenticate"],
        "Bearer error=\"insufficient_scope\", scope=\"message:send\""
    );
    assert_eq!(
        drive(denied.json::<Value>()).await.unwrap(),
        json!({"error":"insufficient_scope"})
    );
    assert_eq!(
        h.fixture.home_snapshot().unwrap(),
        json!({
            "calls":[{"name":"endpoint_query","arguments":{}}],"lights_on":[true,true],"plug_on":true
        })
    );
    h.fixture.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn t3_portos_home_rejects_invalid_bearers_before_scope_or_effects() {
    let h = HomeHarness::linked(echo_config::McpConfig::default().scopes).await;
    let access = drive(h.client.access_token()).await.unwrap();
    let (_, claims, _, _) = mcp_proof::decode(access.expose()).unwrap();
    let mut wrong_audience = claims.clone();
    wrong_audience["aud"] = json!("http://127.0.0.1:1/mcp");
    let mut expired = claims;
    expired["exp"] = json!(1_800_000_005);
    let tokens = {
        let state = h.fixture.shared.lock().unwrap();
        [
            "invalid".into(),
            mcp_tokens::sign(&state, &wrong_audience),
            mcp_tokens::sign(&state, &expired),
        ]
    };
    for token in tokens {
        let reply = drive(
            http()
                .post(format!("{}/mcp", h.fixture.base()))
                .bearer_auth(token)
                .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":control()}))
                .send(),
        )
        .await
        .unwrap();
        assert_eq!(reply.status(), 401);
        assert!(!reply.headers().contains_key("www-authenticate"));
    }
    h.fixture.shared.lock().unwrap().revoked = true;
    let revoked = h.request("tools/call", control()).await;
    assert_eq!(revoked.status(), 401);
    assert_eq!(
        h.fixture.home_snapshot().unwrap(),
        json!({"calls":[],"lights_on":[true,true],"plug_on":true})
    );
    h.fixture.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn t3_portos_per_tool_and_event_scopes_are_checked_before_dispatch() {
    let cases = [
        ("control", "message:send"),
        ("endpoint_query", "endpoint:read"),
        ("endpoint_show", "endpoint:read"),
        ("endpoint_manage", "endpoint:write"),
        ("group_query", "group:read"),
        ("group_manage", "group:manage"),
        ("subscription_list", "subscription:read"),
        ("subscription_inspect", "subscription:read"),
        ("subscription_cancel", "subscription:write"),
        ("subscription_label_edit", "subscription:write"),
        ("events/list", "subscription:read"),
        ("events/subscribe", "subscription:write"),
        ("events/unsubscribe", "subscription:write"),
    ];
    for (tool, missing) in cases {
        let scopes = echo_config::McpConfig::default()
            .scopes
            .into_iter()
            .filter(|s| s != missing)
            .collect();
        let h = HomeHarness::linked(scopes).await;
        let (method, params) = if tool.starts_with("events/") {
            (tool, json!({}))
        } else {
            ("tools/call", json!({"name":tool,"arguments":{}}))
        };
        let denied = h.request(method, params).await;
        assert_eq!(denied.status(), 403, "{tool}");
        assert_eq!(
            denied.headers()["www-authenticate"],
            format!("Bearer error=\"insufficient_scope\", scope=\"{missing}\"")
        );
        assert_eq!(
            h.fixture.home_snapshot().unwrap(),
            json!({"calls":[],"lights_on":[true,true],"plug_on":true})
        );
        h.fixture.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn t3_portos_full_grant_resolves_and_controls_only_twin_room() {
    let h = HomeHarness::linked(echo_config::McpConfig::default().scopes).await;
    let listed: Value = drive(h.request("tools/list", json!({})).await.json()).await.unwrap();
    let names: Vec<_> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "control",
            "endpoint_query",
            "endpoint_show",
            "endpoint_manage",
            "group_query",
            "group_manage",
            "guide",
            "subscription_list",
            "subscription_inspect",
            "subscription_cancel",
            "subscription_label_edit"
        ]
    );
    let found: Value = drive(
        h.request(
            "tools/call",
            json!({"name":"group_query","arguments":{"query":"Twin room"}}),
        )
        .await
        .json(),
    )
    .await
    .unwrap();
    let room: Value = serde_json::from_str(found["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(room["groups"][0]["id"], control()["arguments"]["id"]);
    assert_eq!(room["groups"][0]["messages"], json!(["power/messages/turn-off"]));
    let controlled: Value = drive(h.request("tools/call", control()).await.json()).await.unwrap();
    assert_eq!(
        controlled["result"],
        json!({"content":[{"type":"text","text":"{\"status\":\"accepted\"}"}],"isError":false})
    );
    assert_eq!(
        h.fixture.home_snapshot().unwrap(),
        json!({"calls":[
        {"name":"group_query","arguments":{"query":"Twin room"}},control()
    ],"lights_on":[false,false],"plug_on":true})
    );
    h.fixture.shutdown().await.unwrap();
}
