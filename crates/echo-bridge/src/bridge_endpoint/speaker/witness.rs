//! T3: real Settings, gateway and uplink, with Wyoming and in-memory device peers.
use super::{
    command_tests::{assert_reply, command, receipts, send},
    voice_tests::{Device, Fixture, fixture, fixture_with_link, message},
    *,
};
use echo_link::{
    SpeakerRequest, SpeakerState,
    fake::{Command, FakeConnector, Observer},
};

#[tokio::test(start_paused = true)]
async fn t3_endpoint_speaker_wyoming_refuses_unacknowledged_controls() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(bridge_mcp::bridge_component::drive(async {
            let home = tempfile::tempdir().unwrap();
            let mock = Mock::bind_at_epoch(EPOCH).await.unwrap();
            mock.enable_endpoint_audience().unwrap();
            let mut grant = registered(&mock, home.path()).await;
            let mut gateway = Gateway::open().await;
            let Fixture {
                settings,
                mut device,
                app: _app,
                ..
            } = fixture(home.path(), &mock, &gateway, grant.grant.clone()).await;
            settings.save(true, "Hall Echo").await;
            gateway.assert_upgrade(4).await;
            let report = message(&mock, "report-state", "state", &json!("idle")).await;
            assert_speaker(&report, &Value::Null, &Value::Null);

            for (name, id, payload, event) in [
                ("set-volume", "volume", json!({"volume":40}), "hassmic-set-volume"),
                ("set-mute", "mute", json!({"mute":true}), "hassmic-set-output-mute"),
            ] {
                send_control(&gateway, name, id, &payload).await;
                assert_control(&mut device, event, &payload).await;
                unsupported(&mock, id, name).await;
            }
            rejected_controls(&gateway, &mock).await;
            advance(Duration::from_secs(1)).await;
            assert_eq!(device.next().await.header.r#type, "describe");
            device.send("info", json!({})).await;

            assert_eq!(receipts(&mock).len(), 5);
            for report in reports(&mock) {
                assert_speaker(&report, &Value::Null, &Value::Null);
                assert_eq!(report["state"], "idle");
            }
            settings.close().await;
            gateway.assert_disconnect().await;
            grant.close().await;
        })))
        .await;
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_speaker_uses_reported_volume_and_independent_mic_privacy() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(bridge_mcp::bridge_component::drive(async {
            let home = tempfile::tempdir().unwrap();
            let mock = Mock::bind_at_epoch(EPOCH).await.unwrap();
            mock.enable_endpoint_audience().unwrap();
            let mut grant = registered(&mock, home.path()).await;
            let mut gateway = Gateway::open().await;
            let observer = Observer::default();
            let (settings, _app, ready, ordered) = fixture_with_link(
                home.path(),
                &mock,
                &gateway,
                grant.grant.clone(),
                FakeConnector::new(observer.clone()),
                None,
            )
            .await;
            ready.notified().await;
            settings.save(true, "Hall Echo").await;
            gateway.assert_upgrade(4).await;
            let _ = message(&mock, "report-state", "state", &json!("idle")).await;
            observer.report_speaker(SpeakerState {
                volume: Some(90),
                muted: Some(true),
            });
            let report = projected_report(&mock, "volume", &json!(90)).await;
            assert_speaker(&report, &json!(90), &json!(true));
            assert_eq!(report["state"], "idle");

            send_control(&gateway, "adjust-volume", "relative", &json!({"delta":20})).await;
            unsupported(&mock, "relative", "adjust-volume").await;
            observer.mute(true);
            ordered.notified().await;
            let report = projected_report(&mock, "state", &json!("muted")).await;
            assert_speaker(&report, &json!(90), &json!(true));
            assert_eq!(receipts(&mock).len(), 1);
            assert_eq!(
                observer
                    .commands()
                    .into_iter()
                    .filter(|command| matches!(command, Command::Speaker(_)))
                    .collect::<Vec<_>>(),
                [Command::Speaker(SpeakerRequest::Volume(100))]
            );

            settings.close().await;
            gateway.assert_disconnect().await;
            grant.close().await;
        })))
        .await;
}

async fn rejected_controls(gateway: &Gateway, mock: &Mock) {
    send_control(gateway, "adjust-volume", "no-base", &json!({"delta":-5})).await;
    unsupported(mock, "no-base", "adjust-volume").await;
    for (id, payload) in [("out-of-range", json!({"volume":101})), ("array", json!([40]))] {
        send_control(gateway, "set-volume", id, &payload).await;
        assert_reply(
            mock,
            id,
            "set-volume",
            "INVALID_REQUEST",
            "The command payload is invalid.",
        )
        .await;
    }
}

async fn send_control(gateway: &Gateway, name: &str, id: &str, payload: &Value) {
    let mut frame = command(name, id, ROUTE, payload);
    frame["header"]["namespace"] = json!("speaker");
    send(gateway, &frame).await;
}

async fn assert_control(device: &mut Device, name: &str, payload: &Value) {
    loop {
        let frame = device.next().await;
        if frame.header.r#type == "describe" {
            device.send("info", json!({})).await;
            continue;
        }
        assert_eq!(frame.header.r#type, name);
        assert_eq!(frame.header.data.or(frame.data).as_ref(), Some(payload));
        assert_eq!(frame.payload, Vec::<u8>::new());
        return;
    }
}

async fn unsupported(mock: &Mock, id: &str, name: &str) {
    assert_reply(
        mock,
        id,
        name,
        "NOT_SUPPORTED",
        "The device did not acknowledge speaker control.",
    )
    .await;
}

fn reports(mock: &Mock) -> Vec<Value> {
    mock.state
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter_map(|request| request.json.as_ref())
        .filter(|body| body["header"]["name"] == "report-state")
        .map(|body| body["payload"].clone())
        .collect()
}

async fn projected_report(mock: &Mock, field: &str, expected: &Value) -> Value {
    let finish = Instant::now() + Duration::from_secs(5);
    let drive_clock = async {
        while Instant::now() < finish {
            advance(Duration::from_millis(1)).await;
        }
    };
    tokio::select! {
        report = message(mock, "report-state", field, expected) => report,
        () = drive_clock => panic!("missing speaker projection {field}={expected}"),
    }
}

fn assert_speaker(report: &Value, volume: &Value, muted: &Value) {
    assert_eq!(report.get("volume"), Some(volume));
    assert_eq!(report.get("muted"), Some(muted));
}
