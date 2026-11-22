//! T3: speaker acks against a state-reporting in-memory device, the strict
//! Port OS fake and the loopback gateway.
use super::{
    command_tests::receipts,
    speaker_tests::{
        ack, assert_speaker, assert_speaker_ack, echo_speaker, projected_report, refused, send_control,
        speaker_commands, traced_acks,
    },
    voice_tests::{fixture_with_link, message},
    *,
};
use echo_link::{
    SpeakerRequest, SpeakerState,
    fake::{Command, FakeConnector, Observer},
};

struct Bench {
    home: tempfile::TempDir,
    mock: Mock,
    grant: bridge_mcp::Runtime,
    gateway: Gateway,
    settings: Settings,
    observer: Observer,
    _app: tokio::io::BufReader<tokio::io::DuplexStream>,
}

/// A paired, connected endpoint whose device reports volume 20, unmuted.
async fn bench() -> Bench {
    let home = tempfile::tempdir().unwrap();
    let mock = Mock::bind_at_epoch(EPOCH).await.unwrap();
    mock.enable_endpoint_audience().unwrap();
    let grant = registered(&mock, home.path()).await;
    let mut gateway = Gateway::open(&mock).await;
    let observer = Observer::default();
    let link = FakeConnector::new(observer.clone());
    let (settings, app, ready, _) =
        fixture_with_link(home.path(), &mock, &gateway, grant.grant.clone(), link, None, true).await;
    ready.notified().await;
    settings.save(true, "Hall Echo").await;
    gateway.assert_upgrade(CLAIMED).await;
    observer.report_speaker(SpeakerState {
        volume: Some(20),
        muted: Some(false),
    });
    let report = projected_report(&mock, "/speaker/volume", &json!(20)).await;
    assert!(report.get("requestId").is_none());
    Bench {
        home,
        mock,
        grant,
        gateway,
        settings,
        observer,
        _app: app,
    }
}

impl Bench {
    async fn close(mut self) {
        self.settings.close().await;
        self.gateway.assert_disconnect().await;
        self.grant.close().await;
    }

    async fn acks(&self, count: usize) -> Vec<Value> {
        traced_acks(self.home.path(), count).await
    }
}

/// The first plain state report, without `requestId`, carrying `speaker`.
async fn plain_state(mock: &Mock, speaker: &Value) -> Value {
    let finish = Instant::now() + Duration::from_secs(5);
    loop {
        let found = mock
            .state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|request| request.path == "/messages")
            .filter_map(|request| request.json.as_ref())
            .map(|body| body["payload"].clone())
            .find(|payload| payload.get("requestId").is_none() && &payload["speaker"] == speaker);
        if let Some(payload) = found {
            return payload;
        }
        assert!(Instant::now() < finish, "no plain state report with {speaker}");
        advance(Duration::from_millis(1)).await;
    }
}

/// Each speaker command is acked by one report: its `requestId` and the state
/// the device reported back, with no other claim; a device that reports state
/// but never reflects the change is `NOT_READY` once the settle window passes.
#[tokio::test(start_paused = true)]
async fn t3_endpoint_speaker_ack_carries_the_reported_state() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(bridge_mcp::bridge_component::drive(async {
            let bench = bench().await;
            let sent = Instant::now();
            send_control(&bench.gateway, "set-volume", "volume", &json!({"volume":35})).await;
            echo_speaker(&bench.observer, SpeakerRequest::Volume(35), (35, false)).await;
            let answer = projected_report(&bench.mock, "/requestId", &json!("volume")).await;
            assert_speaker_ack(&answer, "volume", &json!({"volume":35,"muted":false}));
            assert!(sent.elapsed() < Duration::from_secs(1), "an echoed state acks at once");
            send_control(&bench.gateway, "set-mute", "mute", &json!({"mute":true})).await;
            echo_speaker(&bench.observer, SpeakerRequest::OutputMute(true), (35, true)).await;
            let answer = projected_report(&bench.mock, "/requestId", &json!("mute")).await;
            assert_speaker_ack(&answer, "mute", &json!({"volume":35,"muted":true}));
            // The plain state report that follows carries the same state, unsolicited.
            let state = plain_state(&bench.mock, &json!({"volume":35,"muted":true})).await;
            assert_eq!(state["assistant"], json!({"state":"IDLE"}));

            let sent = Instant::now();
            send_control(&bench.gateway, "set-volume", "ignored", &json!({"volume":50})).await;
            let answer = projected_report(&bench.mock, "/requestId", &json!("ignored")).await;
            assert_eq!(answer, refused("ignored", "NOT_READY"));
            assert!(sent.elapsed() >= speaker::SETTLE_WINDOW);
            assert!(sent.elapsed() < speaker::SETTLE_WINDOW + Duration::from_secs(1));

            assert_eq!(receipts(&bench.mock).len(), 3);
            assert_no_refusals(&bench.mock);
            assert_eq!(
                bench.acks(3).await,
                [
                    json!(["answered", "OK", null]),
                    json!(["answered", "OK", null]),
                    json!(["answered", "NOT_READY", null])
                ]
            );
            bench.close().await;
        })))
        .await;
}

/// No caller waits for this ack (fire-and-forget, or past 60 s): Port OS
/// applies its state and answers 404, and the ack is done, never retried.
#[tokio::test(start_paused = true)]
async fn t3_endpoint_speaker_ack_nobody_waits_for_is_done() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(bridge_mcp::bridge_component::drive(async {
            let bench = bench().await;
            bench.gateway.unwaited("late");
            send_control(&bench.gateway, "set-volume", "late", &json!({"volume":60})).await;
            echo_speaker(&bench.observer, SpeakerRequest::Volume(60), (60, false)).await;
            let answer = projected_report(&bench.mock, "/requestId", &json!("late")).await;
            assert_speaker_ack(&answer, "late", &json!({"volume":60,"muted":false}));
            assert_eq!(bench.acks(1).await, [json!(["not_waiting", "OK", 404])]);
            let settle = Instant::now() + Duration::from_secs(15);
            while Instant::now() < settle {
                advance(Duration::from_millis(10)).await;
            }
            let posts = receipts(&bench.mock).len();
            assert_eq!(posts, 1, "a 404 ack is never retried");
            assert!(ack(&bench.mock, "late").is_some());
            assert_eq!(bench.acks(2).await, [json!(["not_waiting", "OK", 404])], "traced once");
            let refusals = bench.mock.state.lock().unwrap().endpoint.refusals.clone();
            assert_eq!(refusals, ["requestId answers no open command"]);
            bench.close().await;
        })))
        .await;
}

/// State reports go out only over a live gateway connection: a change while
/// the socket is gone waits for the reconnect, which reports it.
#[tokio::test(start_paused = true)]
async fn t3_endpoint_reports_nothing_while_the_gateway_is_disconnected() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(bridge_mcp::bridge_component::drive(async {
            let mut bench = bench().await;
            let posted = || {
                bench
                    .mock
                    .state
                    .lock()
                    .unwrap()
                    .requests
                    .iter()
                    .filter(|request| request.path == "/messages")
                    .count()
            };
            // 4503 backs off for at least a second before reconnecting.
            bench.gateway.close(4503).await;
            bench.gateway.assert_disconnect().await;
            let before = posted();
            bench.observer.report_speaker(SpeakerState {
                volume: Some(70),
                muted: Some(false),
            });
            let quiet = Instant::now() + Duration::from_millis(900);
            while Instant::now() < quiet {
                advance(Duration::from_millis(10)).await;
            }
            assert_eq!(posted(), before, "no report while disconnected");
            assert!(bench.gateway.received.try_recv().is_err(), "still disconnected");

            let (gateway, mock) = (&mut bench.gateway, &bench.mock);
            let reconnect = async {
                gateway.assert_upgrade(CLAIMED).await;
                message(mock, "/speaker/volume", &json!(70)).await
            };
            let finish = Instant::now() + Duration::from_secs(5);
            let drive_clock = async {
                while Instant::now() < finish {
                    advance(Duration::from_millis(10)).await;
                }
            };
            let report = tokio::select! {
                report = reconnect => report,
                () = drive_clock => panic!("no reconnect report"),
            };
            assert_speaker(&report, Some(&json!({"volume":70,"muted":false})));
            assert!(report.get("requestId").is_none());
            assert_eq!(speaker_commands(&bench.observer), Vec::<Command>::new());
            assert_no_refusals(&bench.mock);
            bench.close().await;
        })))
        .await;
}
