use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
struct DelayedRoute(AtomicBool);

impl RouteResolver for DelayedRoute {
    fn resolve<'a>(&'a self, auth_link_id: &'a str, tokens: &'a dyn EndpointTokenSource) -> Resolve<'a> {
        if self.0.load(Ordering::Acquire) {
            Resolver.resolve(auth_link_id, tokens)
        } else {
            Box::pin(async { None })
        }
    }
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_pair_resumes_route_lookup_without_a_new_exchange() {
    bridge_mcp::bridge_component::drive(async {
        let home = tempfile::tempdir().unwrap();
        let mock = Mock::bind().await.unwrap();
        mock.enable_endpoint_audience().unwrap();
        let mut grant = registered(&mock, home.path()).await;
        let mut gateway = Gateway::open().await;
        let resolver = Arc::new(DelayedRoute::default());
        let settings = Settings::with_resolver(
            home.path(),
            &mock,
            &gateway,
            grant.grant.clone(),
            resolver.clone(),
            false,
        )
        .await;
        settings.save(true, "Hall Echo").await;
        wait_unavailable(&settings).await;
        let retained = read_install(home.path());
        assert_eq!(
            retained["installation"],
            json!({"authLinkId":LINK, "deviceName":"Hall Echo"})
        );
        assert!(gateway.received.try_recv().is_err());

        settings.save(false, "Hall Echo").await;
        resolver.0.store(true, Ordering::Release);
        settings.save(true, "Hall Echo").await;
        gateway.assert_upgrade(4).await;
        assert_installed(home.path(), &mock).await;
        let install_requests = mock
            .state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|request| request.path == "/plugin-auth-code-exchanges")
            .count();
        assert_eq!(install_requests, 1);
        settings.close().await;
        grant.close().await;
    })
    .await;
}

async fn wait_unavailable(settings: &Settings) {
    for _ in 0..50 {
        if settings.snapshot().await["warnings"] == json!([UNAVAILABLE]) {
            return;
        }
    }
    panic!("endpoint route refusal was not shown in Settings");
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_pair_starts_from_enabled_base_configuration() {
    bridge_mcp::bridge_component::drive(async {
        let home = tempfile::tempdir().unwrap();
        let mock = Mock::bind().await.unwrap();
        mock.enable_endpoint_audience().unwrap();
        let mut grant = registered(&mock, home.path()).await;
        let mut gateway = Gateway::open().await;
        let settings = Settings::with_resolver(
            home.path(),
            &mock,
            &gateway,
            grant.grant.clone(),
            Arc::new(Resolver),
            true,
        )
        .await;
        gateway.assert_upgrade(4).await;
        assert_eq!(settings.snapshot().await["portos"]["endpoint_enabled"], true);
        assert_installed(home.path(), &mock).await;
        assert!(!home.path().join("settings/settings.toml").exists());
        settings.close().await;
        grant.close().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_pair_retries_the_committed_exchange_after_losing_its_response() {
    bridge_mcp::bridge_component::drive(async {
        let home = tempfile::tempdir().unwrap();
        let mock = Mock::bind().await.unwrap();
        mock.enable_endpoint_audience().unwrap();
        let mut grant = registered(&mock, home.path()).await;
        let mut gateway = Gateway::open().await;
        let settings = Settings::open(home.path(), &mock, &gateway, grant.grant.clone()).await;
        mock.state.lock().unwrap().hold = Some("/plugin-auth-code-exchanges".into());
        settings.save(true, "Hall Echo").await;
        while !mock.state.lock().unwrap().held {
            tokio::task::yield_now().await;
        }
        let checkpoint = read_install(home.path());
        assert_eq!(checkpoint["pendingExchange"]["deviceName"], "Hall Echo");
        assert_eq!(checkpoint["installation"], Value::Null);
        assert!(gateway.received.try_recv().is_err());

        settings.close().await;
        mock.state.lock().unwrap().hold = None;
        mock.release.notify_one();
        let settings = Settings::open(home.path(), &mock, &gateway, grant.grant.clone()).await;
        gateway.assert_upgrade(4).await;
        assert_installed(home.path(), &mock).await;
        assert_eq!(read_install(home.path())["pendingExchange"], Value::Null);
        {
            let state = mock.state.lock().unwrap();
            let attempts: Vec<_> = state
                .requests
                .iter()
                .filter(|request| request.path == "/plugin-auth-code-exchanges")
                .collect();
            assert_eq!(attempts.len(), 2);
            assert_eq!(attempts[0].json, attempts[1].json);
            for attempt in attempts {
                assert_eq!(
                    attempt.headers["idempotency-key"],
                    checkpoint["pendingExchange"]["idempotencyKey"]
                );
            }
        }
        settings.close().await;
        grant.close().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_pair_requires_the_existing_port_os_link() {
    bridge_mcp::bridge_component::drive(async {
        let home = tempfile::tempdir().unwrap();
        let mock = Mock::bind().await.unwrap();
        mock.enable_endpoint_audience().unwrap();
        let mut grant = grant_owner(&mock, home.path()).await;
        let mut gateway = Gateway::open().await;
        let settings = Settings::open(home.path(), &mock, &gateway, grant.grant.clone()).await;
        settings.save(true, "Hall Echo").await;
        for _ in 0..50 {
            let state = settings.snapshot().await;
            if state["warnings"] == json!(["Link this Echo with its own Port OS client to connect the endpoint."]) {
                assert_eq!(state["portos"]["challenge"], Value::Null);
                assert_eq!(state["portos"]["link_status"], "link_required");
                assert!(gateway.received.try_recv().is_err());
                assert!(!mock.state.lock().unwrap().requests.iter().any(|request| {
                    matches!(
                        request.path.as_str(),
                        "/custom/link" | "/custom/token" | "/plugin-auth-code-exchanges"
                    )
                }));
                settings.close().await;
                grant.close().await;
                return;
            }
        }
        panic!("endpoint did not ask the operator to link Port OS");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_pair_without_a_production_resolver_keeps_settings_and_mcp_available() {
    bridge_mcp::bridge_component::drive(async {
        let home = tempfile::tempdir().unwrap();
        let mock = Mock::bind().await.unwrap();
        mock.enable_endpoint_audience().unwrap();
        let mut grant = registered(&mock, home.path()).await;
        let (owner, _) = settings_owner(home.path(), &mock, false);
        let (service, send) = SettingsService::resume(Box::new(owner));
        let settings = Settings::serve(service, send, grant.grant.clone()).await;
        settings.save(true, "Kitchen").await;
        let view = settings.snapshot().await;
        assert_eq!(view["portos"]["endpoint_name"], "Kitchen");
        assert_eq!(view["portos"]["endpoint_enabled"], true);
        assert_eq!(view["portos"]["link_status"], "linked");
        assert_eq!(view["warnings"], json!([UNAVAILABLE]));
        advance(Duration::from_secs(61)).await;
        assert_eq!(settings.snapshot().await["portos"]["challenge"], Value::Null);
        assert_eq!(
            mock.state
                .lock()
                .unwrap()
                .requests
                .iter()
                .filter(|request| {
                    request.path == "/custom/link"
                        || request.path == "/plugin-auth-code-exchanges"
                        || request.path == "/messages"
                })
                .map(|request| request.path.as_str())
                .collect::<Vec<_>>(),
            ["/custom/link"]
        );
        assert!(!home.path().join("portos-endpoint").exists());
        settings.close().await;
        grant.close().await;
    })
    .await;
}

pub(super) async fn assert_rest_audiences(mock: &Mock) {
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for route in ["/plugin-auth-code-exchanges", "/messages"] {
        let response = client
            .post(format!("{}{route}", mock.base))
            .bearer_auth("access-5")
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
    }
    for (token, status) in [("access-5", 200), ("endpoint-access-6", 401)] {
        let response = client
            .post(format!("{}/mcp", mock.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
    let body = mock
        .state
        .lock()
        .unwrap()
        .requests
        .iter()
        .find(|request| request.path == "/plugin-auth-code-exchanges")
        .and_then(|request| request.json.clone())
        .unwrap();
    let duplicate = client
        .post(format!("{}/plugin-auth-code-exchanges", mock.base))
        .bearer_auth("endpoint-access-6")
        .header("Idempotency-Key", echo_mcp_auth::new_request_id().unwrap())
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), 409);
}

pub(super) async fn assert_wrong_gateway_audience(gateway: &Gateway) {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let address = gateway.template.replace("{authLinkId}", "loopback%20link%2F%25");
    let mut handshake = address.into_client_request().unwrap();
    handshake
        .headers_mut()
        .insert("Authorization", "Bearer access-5".parse().unwrap());
    let error = tokio_tungstenite::connect_async(handshake).await.unwrap_err();
    assert!(matches!(error, tokio_tungstenite::tungstenite::Error::Http(reply) if reply.status() == 401));
}
