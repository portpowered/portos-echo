use super::*;

#[test]
fn endpoint_api_uses_the_configured_grant_origin_when_omitted() {
    let mut config = BridgeConfig::from_toml_str(include_str!("../../../../examples/bridge.echo.toml")).unwrap();
    config.mcp.server[0].url = "https://api.example.invalid:8443/mcp".into(); // lint-rules: allow-literal offline origin derivation fixture
    assert_eq!(
        endpoint_api(&config).as_deref(),
        Some("https://api.example.invalid:8443") // lint-rules: allow-literal offline origin derivation assertion
    );
    config.portos_endpoint.api_base = Some("https://endpoint.example.invalid".into()); // lint-rules: allow-literal offline origin override fixture
    assert_eq!(
        endpoint_api(&config).as_deref(),
        Some("https://endpoint.example.invalid") // lint-rules: allow-literal offline origin override assertion
    );
    config.portos_endpoint.api_base = None;
    config.mcp.server.clear();
    assert_eq!(endpoint_api(&config), None);
}

#[test]
fn production_runtime_defaults_the_port_os_gateway() {
    let mut config = BridgeConfig::from_toml_str(include_str!("../../../../examples/bridge.echo.toml")).unwrap();
    assert_eq!(endpoint_api(&config).as_deref(), Some(echo_config::PORTOS_API_BASE));
    // The Echo's bundle is absent off the device: the endpoint never runs without its roots.
    assert_eq!(Runtime::configured(&config).err(), Some("ca_unavailable"));
    let directory = tempfile::tempdir().unwrap();
    crate::bridge_mcp::bridge_component::certificate(directory.path(), false);
    config.codex.ca_file = Some(directory.path().join("ca.pem"));
    assert!(Runtime::configured(&config).is_ok());
    std::fs::write(directory.path().join("ca.pem"), "not PEM").unwrap();
    assert_eq!(Runtime::configured(&config).err(), Some("endpoint_api_refused"));
    config.codex.ca_file = None;
    assert!(Runtime::configured(&config).is_ok());
    config.portos_endpoint.api_base = Some("https://endpoint.example.invalid".into()); // lint-rules: allow-literal offline origin override fixture
    assert_eq!(Runtime::configured(&config).err(), Some("no_gateway"));
    config.portos_endpoint.gateway_url = Some("wss://gateway.example.invalid/devices/{authLinkId}".into()); // lint-rules: allow-literal offline gateway override fixture
    assert!(Runtime::configured(&config).is_ok());
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_serves_while_route_discovery_lags() {
    bridge_mcp::bridge_component::drive(async {
        let home = tempfile::tempdir().unwrap();
        let mock = Mock::bind_at_epoch(EPOCH).await.unwrap();
        mock.enable_endpoint_audience().unwrap();
        mock.state.lock().unwrap().endpoint.route_delay = 2;
        let mut grant = registered(&mock, home.path()).await;
        let mut gateway = Gateway::open(&mock).await;
        let settings = Settings::open(home.path(), &mock, &gateway, grant.grant.clone()).await;
        settings.save(true, "Hall Echo").await;
        gateway.assert_upgrade(CLAIMED).await;
        let pending = read_install(home.path());
        assert_eq!(
            pending["installation"],
            json!({"authLinkId":LINK, "deviceName":"Hall Echo"})
        );
        // Discovery retries after 5 s and 30 s; drive the clock until the third read.
        let routes = || route_reads(&mock);
        let deadline = Instant::now() + Duration::from_secs(40);
        while routes() < 3 {
            assert!(Instant::now() < deadline, "route discovery retries");
            advance(Duration::from_millis(10)).await;
        }
        assert_installed(home.path(), &mock).await;
        let (exchanges, reads) = {
            let state = mock.state.lock().unwrap();
            let count = |path: &str| state.requests.iter().filter(|request| request.path == path).count();
            (count("/plugin-auth-code-exchanges"), count("/routes"))
        };
        assert_eq!((exchanges, reads), (1, 3));
        settings.close().await;
        grant.close().await;
    })
    .await;
}

fn route_reads(mock: &Mock) -> usize {
    let state = mock.state.lock().unwrap();
    state
        .requests
        .iter()
        .filter(|request| request.path == "/routes")
        .count()
}

pub(super) async fn wait_warning(settings: &Settings, warning: &str) -> Value {
    let expected = json!([warning]);
    for _ in 0..50 {
        let snapshot = settings.snapshot().await;
        if snapshot["warnings"] == expected {
            return snapshot;
        }
    }
    let snapshot = settings.snapshot().await;
    assert_eq!(snapshot["warnings"], expected);
    snapshot
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_pair_starts_from_enabled_base_configuration() {
    bridge_mcp::bridge_component::drive(async {
        let home = tempfile::tempdir().unwrap();
        let mock = Mock::bind_at_epoch(EPOCH).await.unwrap();
        mock.enable_endpoint_audience().unwrap();
        let mut grant = registered(&mock, home.path()).await;
        let mut gateway = Gateway::open(&mock).await;
        let settings = Settings::with_base(home.path(), &mock, &gateway, grant.grant.clone(), true).await;
        gateway.assert_upgrade(CLAIMED).await;
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
        let mock = Mock::bind_at_epoch(EPOCH).await.unwrap();
        mock.enable_endpoint_audience().unwrap();
        let mut grant = registered(&mock, home.path()).await;
        let mut gateway = Gateway::open(&mock).await;
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
        gateway.assert_upgrade(CLAIMED).await;
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
        let mock = Mock::bind_at_epoch(EPOCH).await.unwrap();
        mock.enable_endpoint_audience().unwrap();
        let mut grant = grant_owner(&mock, home.path(), EPOCH).await;
        let mut gateway = Gateway::open(&mock).await;
        let settings = Settings::open(home.path(), &mock, &gateway, grant.grant.clone()).await;
        settings.save(true, "Hall Echo").await;
        let state = wait_warning(
            &settings,
            "Link this Echo with its own Port OS client to connect the endpoint.",
        )
        .await;
        assert_eq!(state["portos"]["challenge"], Value::Null);
        assert_eq!(state["portos"]["link_status"], "link_required");
        assert!(gateway.received.try_recv().is_err());
        let rows = wait_event(home.path(), "endpoint.relink_waiting").await;
        assert_eq!(
            steps(&rows),
            [json!({"event":"endpoint.relink_waiting","reason":"unlinked"})]
        );
        advance(Duration::from_secs(5)).await;
        assert_eq!(
            steps(&lifecycle(home.path())).len(),
            1,
            "waiting is traced once per reason"
        );
        assert!(!mock.state.lock().unwrap().requests.iter().any(|request| {
            matches!(
                request.path.as_str(),
                "/custom/link" | "/custom/token" | "/plugin-auth-code-exchanges"
            )
        }));
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

/// A token without the auth-link claim is admitted to the upgrade and then
/// closed with 4403, as the live gateway does.
pub(super) async fn assert_unclaimed_gateway_token(gateway: &mut Gateway) {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let address = gateway.template.replace("{authLinkId}", "loopback%20link%2F%25");
    let mut handshake = address.into_client_request().unwrap();
    handshake
        .headers_mut()
        .insert("Authorization", "Bearer endpoint-access-2".parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(handshake).await.unwrap();
    let close = loop {
        match futures_util::StreamExt::next(&mut socket).await {
            Some(Ok(tokio_tungstenite::tungstenite::Message::Close(frame))) => break frame,
            Some(Ok(_)) => {}
            _ => break None,
        }
    };
    assert_eq!(close.map(|frame| u16::from(frame.code)), Some(4403));
    assert_eq!(
        gateway.received.recv().await,
        Some(gateway_test_peer::GatewayEvent::Forbidden(
            "Bearer endpoint-access-2".into()
        ))
    );
}

/// The endpoint lifecycle rows of the settings trace, without the envelope.
pub(super) fn lifecycle(home: &Path) -> Vec<Value> {
    let trace = fs::read_to_string(home.join("settings-trace")).unwrap();
    for secret in [
        DEVICE,
        "loopback link",
        "endpoint-access-",
        "Bearer",
        "refresh-",
        "Hall Echo",
    ] {
        assert_eq!(
            trace.matches(secret).count(),
            0,
            "the trace names no ids, tokens or names"
        );
    }
    trace
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| {
            row["event"]
                .as_str()
                .is_some_and(|event| event.starts_with("endpoint."))
        })
        .map(|mut row| {
            let object = row.as_object_mut().unwrap();
            for envelope in ["elapsed_ms", "provider_auth_mode"] {
                object.remove(envelope);
            }
            row
        })
        .collect()
}

async fn wait_event(home: &Path, event: &str) -> Vec<Value> {
    for _ in 0..3_000 {
        let rows = lifecycle(home);
        if rows.iter().any(|row| row["event"] == event) {
            return rows;
        }
        advance(Duration::from_millis(10)).await;
    }
    let rows = lifecycle(home);
    assert!(rows.iter().any(|row| row["event"] == event), "no {event} in {rows:?}");
    rows
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_pair_retries_server_errors_and_traces_each_transition() {
    bridge_mcp::bridge_component::drive(async {
        let home = tempfile::tempdir().unwrap();
        let mock = Mock::bind_at_epoch(EPOCH).await.unwrap();
        mock.enable_endpoint_audience().unwrap();
        let mut grant = registered(&mock, home.path()).await;
        let mut gateway = Gateway::open(&mock).await;
        let settings = Settings::open(home.path(), &mock, &gateway, grant.grant.clone()).await;
        mock.state.lock().unwrap().fault =
            Some(("/plugin-auth-code-exchanges".into(), 503, json!({"error":"synthetic"})));
        settings.save(true, "Hall Echo").await;
        let rows = wait_event(home.path(), "endpoint.exchange_failed").await;
        assert_eq!(
            steps(&rows),
            [
                json!({"event":"endpoint.linked","waited":false}),
                json!({"event":"endpoint.exchanging","rename":false}),
                json!({"event":"endpoint.exchange_failed","reason":"server_error","http_class":"5xx",
                    "http_status":503,"attempt":1,"retry":true}),
            ]
        );
        wait_warning(&settings, UNAVAILABLE).await;
        assert!(gateway.received.try_recv().is_err());
        mock.state.lock().unwrap().fault = None;
        // The retry waits out its 1 s backoff on the paused clock.
        advance(Duration::from_secs(2)).await;
        gateway.assert_upgrade(CLAIMED).await;
        assert_installed(home.path(), &mock).await;
        wait_warning(&settings, "This Echo is connected to Port OS.").await;
        let rows = wait_event(home.path(), "endpoint.route").await;
        assert_eq!(
            steps(&rows)[3..],
            [
                json!({"event":"endpoint.exchanged","attempts":2}),
                json!({"event":"endpoint.gateway_connecting","fresh_token":false}),
                json!({"event":"endpoint.gateway_connected"}),
                json!({"event":"endpoint.route","result":"found"}),
            ]
        );
        let statuses: Vec<_> = rows.iter().filter_map(|row| row["status"].as_str()).collect();
        assert_eq!(statuses[..2], ["connecting", "unavailable"]);
        assert_eq!(statuses.last(), Some(&"connected"));
        settings.close().await;
        grant.close().await;
    })
    .await;
}

/// The lifecycle steps other than the observed Settings status.
fn steps(rows: &[Value]) -> Vec<Value> {
    rows.iter()
        .filter(|row| row["event"] != "endpoint.status")
        .cloned()
        .collect()
}
