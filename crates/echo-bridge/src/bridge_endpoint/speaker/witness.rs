//! T3: real Settings, gateway and uplink, with Wyoming and in-memory device peers.
use super::{
    command_tests::{command, receipts, send},
    voice_tests::{Device, Fixture, fixture, fixture_with_link, message},
    *,
};
use echo_link::{
    SpeakerRequest, SpeakerState,
    fake::{Command, FakeConnector, Observer},
};

#[tokio::test(start_paused = true)]
async fn t3_endpoint_speaker_controls_are_fire_and_forget() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(bridge_mcp::bridge_component::drive(async {
            let home = tempfile::tempdir().unwrap();
            let mock = Mock::bind_at_epoch(EPOCH).await.unwrap();
            mock.enable_endpoint_audience().unwrap();
            let mut grant = registered(&mock, home.path()).await;
            let mut gateway = Gateway::open(&mock).await;
            let Fixture {
                settings,
                mut device,
                app: _app,
                ..
            } = fixture(home.path(), &mock, &gateway, grant.grant.clone()).await;
            settings.save(true, "Hall Echo").await;
            gateway.assert_upgrade(CLAIMED).await;
            let report = message(&mock, "/assistant/state", &json!("IDLE")).await;
            assert_speaker(&report, None);

            for (name, id, payload, event) in [
                ("set-volume", "volume", json!({"volume":40}), "hassmic-set-volume"),
                ("set-mute", "mute", json!({"mute":true}), "hassmic-set-output-mute"),
            ] {
                send_control(&gateway, name, id, &payload).await;
                assert_control(&mut device, event, &payload).await;
            }
            rejected_controls(&gateway).await;
            advance(Duration::from_secs(1)).await;
            assert_eq!(device.next().await.header.r#type, "describe");
            device.send("info", json!({})).await;

            // Speaker commands never get a reply, accepted or not.
            assert_eq!(receipts(&mock).len(), 0);
            for report in reports(&mock) {
                assert_speaker(&report, None);
                assert_eq!(report["assistant"], json!({"state":"IDLE"}));
            }
            assert_no_refusals(&mock);
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
            let mut gateway = Gateway::open(&mock).await;
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
            gateway.assert_upgrade(CLAIMED).await;
            let _ = message(&mock, "/assistant/state", &json!("IDLE")).await;
            observer.report_speaker(SpeakerState {
                volume: Some(90),
                muted: Some(true),
            });
            let report = projected_report(&mock, "/speaker/volume", &json!(90)).await;
            assert_speaker(&report, Some(&json!({"volume":90,"muted":true})));
            assert_eq!(report["assistant"], json!({"state":"IDLE"}));

            send_control(&gateway, "adjust-volume", "relative", &json!({"volumeDelta":20})).await;
            observer.mute(true);
            ordered.notified().await;
            let report = projected_report(&mock, "/assistant/state", &json!("MUTED")).await;
            assert_speaker(&report, Some(&json!({"volume":90,"muted":true})));
            assert!(report.get("requestId").is_none(), "a speaker effect is plain state");
            assert_eq!(receipts(&mock).len(), 0);
            assert_no_refusals(&mock);
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

/// Refused controls reach no device and get no reply.
async fn rejected_controls(gateway: &Gateway) {
    send_control(gateway, "adjust-volume", "no-base", &json!({"volumeDelta":-5})).await;
    for (id, payload) in [("out-of-range", json!({"volume":101})), ("array", json!([40]))] {
        send_control(gateway, "set-volume", id, &payload).await;
    }
}

async fn send_control(gateway: &Gateway, name: &str, id: &str, payload: &Value) {
    send(gateway, &command(name, id, DEVICE, payload)).await;
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

fn reports(mock: &Mock) -> Vec<Value> {
    mock.state
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|request| request.path == "/messages")
        .filter_map(|request| request.json.as_ref())
        .filter(|body| body["payload"]["assistant"].is_object())
        .map(|body| body["payload"].clone())
        .collect()
}

async fn projected_report(mock: &Mock, pointer: &str, expected: &Value) -> Value {
    let finish = Instant::now() + Duration::from_secs(5);
    let drive_clock = async {
        while Instant::now() < finish {
            advance(Duration::from_millis(1)).await;
        }
    };
    tokio::select! {
        report = message(mock, pointer, expected) => report,
        () = drive_clock => panic!("missing speaker projection {pointer}={expected}"),
    }
}

/// Speaker state is reported only once both volume and mute are known.
fn assert_speaker(report: &Value, speaker: Option<&Value>) {
    assert_eq!(report.get("speaker"), speaker);
}
