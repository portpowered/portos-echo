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

/// Today's hassmic applies volume and mute but reports neither, so nothing
/// can confirm a control: each is acked `NOT_SUPPORTED` once the settle
/// window passes, and refused controls are acked at once.
#[tokio::test(start_paused = true)]
async fn t3_endpoint_speaker_acks_refuse_without_device_state() {
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

            let sent = Instant::now();
            for (name, id, payload, event) in [
                ("set-volume", "volume", json!({"volume":40}), "hassmic-set-volume"),
                ("set-mute", "mute", json!({"mute":true}), "hassmic-set-output-mute"),
            ] {
                send_control(&gateway, name, id, &payload).await;
                assert_control(&mut device, event, &payload).await;
            }
            rejected_controls(&gateway).await;
            for (id, code) in [
                ("no-base", "NOT_SUPPORTED"),
                ("out-of-range", "INVALID_REQUEST"),
                ("array", "INVALID_REQUEST"),
            ] {
                assert_eq!(acks(&mock, &mut device, &[id]).await, [refused(id, code)]);
            }
            let settled = acks(&mock, &mut device, &["volume", "mute"]).await;
            assert_eq!(
                settled,
                [refused("volume", "NOT_SUPPORTED"), refused("mute", "NOT_SUPPORTED")]
            );
            let waited = sent.elapsed();
            assert!(
                waited >= speaker::SETTLE_WINDOW && waited < speaker::SETTLE_WINDOW + Duration::from_secs(1),
                "{waited:?}"
            );

            assert_eq!(receipts(&mock).len(), 5, "one ack per speaker command");
            for report in reports(&mock) {
                assert_speaker(&report, None);
                assert_eq!(report["assistant"], json!({"state":"IDLE"}));
            }
            assert_no_refusals(&mock);
            assert_audited(home.path(), "volume_unknown").await;
            settings.close().await;
            gateway.assert_disconnect().await;
            grant.close().await;
        })))
        .await;
}

/// The audit and ack rows name the ack's code; a control that reached no
/// device is also traced as dropped.
async fn assert_audited(home: &Path, reason: &str) {
    let mut acked: Vec<_> = traced_acks(home, 5).await.iter().map(ToString::to_string).collect();
    acked.sort_unstable();
    let row = |code: &str| json!(["answered", code, null]).to_string();
    let codes = [
        "INVALID_REQUEST",
        "INVALID_REQUEST",
        "NOT_SUPPORTED",
        "NOT_SUPPORTED",
        "NOT_SUPPORTED",
    ];
    assert_eq!(acked, codes.map(row));
    let rows = setup_tests::lifecycle(home);
    let mut audits: Vec<_> = rows
        .iter()
        .filter(|row| row["event"] == "endpoint.command")
        .map(|row| format!("{} {}", row["name"].as_str().unwrap(), row["code"].as_str().unwrap()))
        .collect();
    audits.sort_unstable();
    let expected = [
        "adjust-volume NOT_SUPPORTED",
        "set-mute NOT_SUPPORTED",
        "set-volume INVALID_REQUEST",
        "set-volume INVALID_REQUEST",
        "set-volume NOT_SUPPORTED",
    ];
    assert_eq!(audits, expected);
    let dropped: Vec<_> = rows
        .iter()
        .filter(|row| row["event"] == "endpoint.speaker_dropped")
        .map(|row| row["reason"].clone())
        .collect();
    assert_eq!(dropped, [json!(reason)]);
}

/// The `endpoint.speaker_ack` rows as `[outcome, code, http_status]`, once
/// `count` are traced: each is written after its delivery is answered.
pub(super) async fn traced_acks(home: &Path, count: usize) -> Vec<Value> {
    let finish = Instant::now() + Duration::from_secs(5);
    loop {
        let acks: Vec<_> = setup_tests::lifecycle(home)
            .into_iter()
            .filter(|row| row["event"] == "endpoint.speaker_ack")
            .map(|row| json!([row["outcome"], row["code"], row["http_status"]]))
            .collect();
        if acks.len() >= count || Instant::now() >= finish {
            return acks;
        }
        advance(Duration::from_millis(10)).await;
    }
}

/// A relative change uses the reported volume as its base, and its ack carries
/// the state the device reports back. Speaker state that follows is plain
/// state with no `requestId`; microphone privacy stays independent.
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
                true,
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
            echo_speaker(&observer, SpeakerRequest::Volume(100), (100, true)).await;
            let ack = projected_report(&mock, "/requestId", &json!("relative")).await;
            assert_speaker_ack(&ack, "relative", &json!({"volume":100,"muted":true}));
            observer.mute(true);
            ordered.notified().await;
            let report = projected_report(&mock, "/assistant/state", &json!("MUTED")).await;
            assert_speaker(&report, Some(&json!({"volume":100,"muted":true})));
            assert!(
                report.get("requestId").is_none(),
                "unsolicited speaker state is plain state"
            );
            assert_eq!(receipts(&mock).len(), 1);
            assert_no_refusals(&mock);
            assert_eq!(
                speaker_commands(&observer),
                [Command::Speaker(SpeakerRequest::Volume(100))]
            );

            settings.close().await;
            gateway.assert_disconnect().await;
            grant.close().await;
        })))
        .await;
}

/// Refused controls reach no device.
async fn rejected_controls(gateway: &Gateway) {
    send_control(gateway, "adjust-volume", "no-base", &json!({"volumeDelta":-5})).await;
    for (id, payload) in [("out-of-range", json!({"volume":101})), ("array", json!([40]))] {
        send_control(gateway, "set-volume", id, &payload).await;
    }
}

pub(super) async fn send_control(gateway: &Gateway, name: &str, id: &str, payload: &Value) {
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

/// Drive the clock, answering the device's describe probes, until each of
/// `ids` has its ack; returns the ack payloads in that order.
async fn acks(mock: &Mock, device: &mut Device, ids: &[&str]) -> Vec<Value> {
    let finish = Instant::now() + Duration::from_secs(10);
    loop {
        let found: Vec<_> = ids.iter().filter_map(|id| ack(mock, id)).collect();
        if found.len() == ids.len() {
            return found;
        }
        assert!(Instant::now() < finish, "missing speaker acks {ids:?}");
        tokio::select! {
            frame = device.next() => {
                assert_eq!(frame.header.r#type, "describe");
                device.send("info", json!({})).await;
            }
            () = advance(Duration::from_millis(10)) => {}
        }
    }
}

/// The payload of the one report whose `requestId` is `id`.
pub(super) fn ack(mock: &Mock, id: &str) -> Option<Value> {
    let mut answers: Vec<_> = receipts(mock)
        .into_iter()
        .filter_map(|request| request.json)
        .filter(|body| body["payload"]["requestId"] == id)
        .map(|body| body["payload"].clone())
        .collect();
    assert!(answers.len() <= 1, "one ack per command");
    answers.pop()
}

pub(super) fn refused(id: &str, code: &str) -> Value {
    json!({"requestId":id,"error":{"code":code}})
}

/// A speaker ack: `requestId`, the sample time and the resulting state, no other claim.
pub(super) fn assert_speaker_ack(ack: &Value, id: &str, speaker: &Value) {
    let mut fields: Vec<_> = ack.as_object().unwrap().keys().cloned().collect();
    fields.sort_unstable();
    assert_eq!(fields, ["requestId", "sampledAtMs", "speaker"]);
    assert_eq!(ack["requestId"], id);
    assert_eq!(&ack["speaker"], speaker);
    assert!(ack["sampledAtMs"].is_i64());
}

/// Wait for the fake device to receive `request`, then report the resulting
/// `volume` and `muted` back as a state-reporting hassmic would.
pub(super) async fn echo_speaker(observer: &Observer, request: SpeakerRequest, (volume, muted): (u8, bool)) {
    let finish = Instant::now() + Duration::from_secs(1);
    while !speaker_commands(observer).contains(&Command::Speaker(request)) {
        assert!(Instant::now() < finish, "the device never got {request:?}");
        advance(Duration::from_millis(1)).await;
    }
    observer.report_speaker(SpeakerState {
        volume: Some(volume),
        muted: Some(muted),
    });
}

pub(super) fn speaker_commands(observer: &Observer) -> Vec<Command> {
    observer
        .commands()
        .into_iter()
        .filter(|command| matches!(command, Command::Speaker(_)))
        .collect()
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

pub(super) async fn projected_report(mock: &Mock, pointer: &str, expected: &Value) -> Value {
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
pub(super) fn assert_speaker(report: &Value, speaker: Option<&Value>) {
    assert_eq!(report.get("speaker"), speaker);
}
