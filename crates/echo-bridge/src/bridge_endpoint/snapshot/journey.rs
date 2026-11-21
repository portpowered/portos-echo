//! T3 integrated installation, voice, camera, REST recovery and gateway lifecycle.
use super::{
    command_tests::receipts,
    voice_tests::{Device, app_stop, assert_end, begin_text, play_reply},
    *,
};
use echo_mcp_auth::mock::{MessageFault, Request};
use gateway_test_peer::GatewayEvent;
use tokio_tungstenite::tungstenite::Message;

const INITIAL_TEXT: &str = "integrated-private-first-turn";
const TRANSCRIPT: &str = "integrated-private-transcript";

#[tokio::test(start_paused = true)]
async fn t3_endpoint_integrated() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(bridge_mcp::bridge_component::drive(async {
            let home = tempfile::tempdir().unwrap();
            let mock = Mock::bind_at_epoch(EPOCH).await.unwrap();
            mock.enable_endpoint_audience().unwrap();
            let mut grant = registered(&mock, home.path()).await;
            let mut gateway = Gateway::open().await;
            let camera = CameraPeer::open();
            let mut fixture =
                fixture_with_camera(home.path(), &mock, &gateway, grant.grant.clone(), Some(camera.clone())).await;
            fixture.settings.save(true, "Hall Echo").await;
            gateway.assert_upgrade(4).await;
            assert_installed(home.path(), &mock).await;
            text_and_snapshot(&gateway, &mock, &mut fixture).await;
            assert_malformed_frames(&gateway, &mock).await;
            debounce_and_reconnect(&mut gateway, &mock, &mut fixture).await;
            camera.started(2).await;
            tokio::select! {
                () = retry_and_unlink(home.path(), &mut gateway, &mock, &fixture.settings) => {}
                () = answer_idle_probes(&mut fixture.device) => {}
            }
            assert_private_trace(home.path());
            fixture.settings.close().await;
            grant.close().await;
        })))
        .await;
}

async fn retry_and_unlink(home: &Path, gateway: &mut Gateway, mock: &Mock, settings: &Settings) {
    assert_retry(gateway, mock).await;
    let device_id = read_install(home)["deviceId"].clone();
    settings.unlink().await;
    gateway.assert_disconnect().await;
    assert_unlinked(home, settings, device_id).await;
    for _ in 0..244 {
        advance(Duration::from_millis(250)).await;
        assert!(gateway.received.try_recv().is_err());
    }
}

async fn answer_idle_probes(device: &mut Device) {
    loop {
        let frame = device.next().await;
        assert_eq!(frame.header.r#type, "describe");
        device.send("info", json!({})).await;
    }
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_integrated_gateway_backs_off_after_the_host_ends_its_hour() {
    Box::pin(bridge_mcp::bridge_component::drive(async {
        let home = tempfile::tempdir().unwrap();
        let mock = Mock::bind_at_epoch(EPOCH).await.unwrap();
        mock.enable_endpoint_audience().unwrap();
        mock.state.lock().unwrap().lifetime = 7200;
        let mut grant = registered(&mock, home.path()).await;
        let mut gateway = Gateway::open().await;
        let settings = Settings::open(home.path(), &mock, &gateway, grant.grant.clone()).await;
        settings.save(true, "Hall Echo").await;
        gateway.assert_upgrade(4).await;
        assert_initial_report(&mock).await;
        advance(Duration::from_secs(3600)).await;
        gateway.reject_next(8);
        gateway.frames.send(Message::Close(None)).await.unwrap();
        gateway.assert_disconnect().await;
        setup_tests::wait_warning(&settings, "Connecting this Echo to Port OS.").await;
        assert_backoff(&mut gateway).await;
        tick_until(Duration::from_millis(20), || report_count(&mock, "idle") == 2).await;
        settings.close().await;
        gateway.assert_disconnect().await;
        grant.close().await;
    }))
    .await;
}

async fn assert_backoff(gateway: &mut Gateway) {
    let mut previous = Instant::now();
    for (index, seconds) in [1, 2, 4, 8, 16, 32, 60, 60, 60].into_iter().enumerate() {
        let earliest = Duration::from_secs(seconds);
        let latest = (earliest + Duration::from_millis(250)).min(Duration::from_secs(60));
        // Allow two clock ticks for the HTTP response and socket readiness.
        let ceiling = latest + Duration::from_millis(40);
        while gateway.received.is_empty() {
            assert!(previous.elapsed() < ceiling);
            advance(Duration::from_millis(10)).await;
        }
        let event = gateway.received.try_recv().unwrap();
        if index == 8 {
            assert_eq!(
                event,
                GatewayEvent::Upgrade(
                    "/devices/loopback%20link%2F%25".into(),
                    "Bearer endpoint-access-4".into(),
                    false
                )
            );
            assert!((earliest..=ceiling).contains(&previous.elapsed()));
        } else {
            let at = if let GatewayEvent::Rejected(at) = event {
                Some(at)
            } else {
                None
            }
            .unwrap();
            assert!((earliest..=ceiling).contains(&(at - previous)));
            previous = at;
        }
    }
}

async fn text_and_snapshot(gateway: &Gateway, mock: &Mock, fixture: &mut Fixture) {
    let opened = begin_text(gateway, mock, &mut fixture.app, "text", false, INITIAL_TEXT).await;
    let session = opened["sessionId"].as_str().unwrap();
    send(
        gateway,
        &command("start-session", "remote-busy", ROUTE, &json!({"reason":"fixture"})),
    )
    .await;
    assert_eq!(
        outcome(mock, "remote-busy").await,
        json!({
            "ok":false,"failure":{"code":"BUSY","message":"A voice session is already active."},
            "sessionId":session,"origin":"remote"
        })
    );
    assert_jpeg(gateway, mock, "snapshot").await;
    play_reply(&mut fixture.app, &mut fixture.device, TRANSCRIPT).await;
    app_stop(&mut fixture.app).await;
    assert_end(mock, session, "remote", "completed").await;
}

async fn assert_retry(gateway: &Gateway, mock: &Mock) {
    script_uplink_faults(mock);
    let expiry = Instant::now() + Duration::from_secs(10);
    unknown(gateway, "expires").await;
    let _ = outcome(mock, "expires").await;
    unknown(gateway, "retry").await;
    unknown(gateway, "terminal").await;
    assert_eq!(
        outcome(mock, "terminal").await["failure"]["code"],
        "UNSUPPORTED_COMMAND"
    );
    tick_until(Duration::from_secs(5), || attempts(mock, "retry").len() == 3).await;
    assert_attempts(&attempts(mock, "retry"), &[Some(503), None, Some(202)]);
    assert_retry_window(gateway, mock, expiry).await;
}

fn script_uplink_faults(mock: &Mock) {
    let mut state = mock.state.lock().unwrap();
    state.message_faults.insert(
        "retry".into(),
        [MessageFault::Status(503), MessageFault::Disconnect].into(),
    );
    state
        .message_faults
        .insert("terminal".into(), [MessageFault::Status(400)].into());
    state
        .message_faults
        .insert("expires".into(), (0..60).map(|_| MessageFault::Status(503)).collect());
}

async fn assert_retry_window(gateway: &Gateway, mock: &Mock, expiry: Instant) {
    while Instant::now() < expiry {
        advance(Duration::from_millis(10)).await;
    }
    let expired = attempts(mock, "expires");
    assert!(expired.len() > 1);
    assert!(expired.iter().all(|request| request.received < expiry));
    assert_attempts(&expired, &vec![Some(503); expired.len()]);
    advance(Duration::from_secs(1)).await;
    unknown(gateway, "later").await;
    assert_eq!(outcome(mock, "later").await["failure"]["code"], "UNSUPPORTED_COMMAND");
    assert_attempts(&attempts(mock, "terminal"), &[Some(400)]);
    assert_attempts(&attempts(mock, "later"), &[Some(202)]);
    assert_eq!(attempts(mock, "expires").len(), expired.len());
}

async fn unknown(gateway: &Gateway, id: &str) {
    send(gateway, &command("future", id, ROUTE, &json!({}))).await;
}

fn attempts(mock: &Mock, id: &str) -> Vec<Request> {
    receipts(mock)
        .into_iter()
        .filter(|request| request.json.as_ref().unwrap()["payload"]["requestId"] == id)
        .collect()
}

fn assert_attempts(attempts: &[Request], statuses: &[Option<u16>]) {
    assert_eq!(
        attempts.iter().map(|request| request.status).collect::<Vec<_>>(),
        statuses
    );
    let first = &attempts[0];
    for request in attempts {
        assert_eq!(request.headers["idempotency-key"], first.headers["idempotency-key"]);
        assert_eq!(request.headers["authorization"], "Bearer endpoint-access-4");
        assert!(!request.headers.contains_key("dpop"));
        assert_eq!(request.message_body, first.message_body);
        assert_eq!(
            request.json.as_ref().unwrap()["target"],
            json!({"type":"ROUTE","value":ROUTE})
        );
    }
}

async fn assert_malformed_frames(gateway: &Gateway, mock: &Mock) {
    gateway.frames.send(Message::text("{")).await.unwrap();
    send(gateway, &snapshot_request("once", "BASE64")).await;
    // Send the right route with an ID already admitted and then a wrong route.
    let mut duplicate = snapshot_request("snapshot", "BASE64");
    send(gateway, &duplicate).await;
    duplicate["header"]["messageId"] = json!("wrong");
    duplicate["target"]["value"] = json!("wrong-route");
    send(gateway, &duplicate).await;
    unknown(gateway, "barrier").await;
    let _ = outcome(mock, "barrier").await;
    assert_eq!(outcome(mock, "once").await["ok"], true);
    assert_eq!(attempts(mock, "snapshot").len(), 1);
    assert!(attempts(mock, "wrong").is_empty());
}

async fn debounce_and_reconnect(gateway: &mut Gateway, mock: &Mock, fixture: &mut Fixture) {
    let reports = |state| report_count(mock, state);
    let before = reports("muted");
    for muted in [true, false, true] {
        fixture.device.send("hassmic-mute", json!({"muted":muted})).await;
        fixture.stale.notified().await;
    }
    assert_eq!(reports("muted"), before);
    tick_until(Duration::from_millis(260), || reports("muted") == before + 1).await;
    gateway.frames.send(Message::Close(None)).await.unwrap();
    gateway.assert_disconnect().await;
    setup_tests::wait_warning(&fixture.settings, "Connecting this Echo to Port OS.").await;
    let disconnected = Instant::now();
    advance(Duration::from_millis(999)).await;
    assert!(gateway.received.try_recv().is_err());
    advance(Duration::from_millis(251)).await;
    gateway.assert_upgrade(4).await;
    assert_eq!(disconnected.elapsed(), Duration::from_millis(1250));
    tick_until(Duration::from_millis(20), || reports("muted") == before + 2).await;
}

fn report_count(mock: &Mock, state: &str) -> usize {
    mock.state
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|request| {
            request.json.as_ref().is_some_and(|body| {
                body["header"]["name"] == "report-state"
                    && body["target"] == json!({"type":"ROUTE","value":ROUTE})
                    && body["payload"]
                        == json!({"state":state,"sessionId":null,"origin":null,"volume":null,"muted":null})
            })
        })
        .count()
}

async fn tick_until(limit: Duration, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + limit;
    while !condition() {
        assert!(Instant::now() < deadline, "expected wire event before virtual deadline");
        advance(Duration::from_millis(1)).await;
    }
}

fn assert_private_trace(home: &Path) {
    let settings = fs::read_to_string(home.join("settings-trace")).unwrap();
    let voice = fs::read_to_string(home.join("voice-trace")).unwrap();
    let grant = fs::read_to_string(home.join("trace")).unwrap();
    for trace in [&settings, &voice, &grant] {
        for marker in [INITIAL_TEXT, TRANSCRIPT, JPEG, "endpoint-access-", "Bearer", "refresh-"] {
            assert_eq!(trace.matches(marker).count(), 0);
        }
        let jpeg = STANDARD.decode(JPEG).unwrap();
        assert!(!trace.as_bytes().windows(jpeg.len()).any(|window| window == jpeg));
    }
    let rows: Vec<Value> = settings
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let audit: Vec<_> = rows.iter().filter(|row| row["event"] == "endpoint.command").collect();
    assert_eq!(
        audit
            .iter()
            .filter(|row| row["name"] == "get-snapshot" && row["code"] == "OK")
            .count(),
        2
    );
    assert!(
        audit
            .iter()
            .all(|row| row.as_object().unwrap().len() == 6 && row["duration_ms"].is_u64())
    );
    assert!(voice.lines().any(|line| {
        let row: Value = serde_json::from_str(line).unwrap();
        row["event"] == "endpoint.session_started" && row["reason"].is_string()
    }));
}
