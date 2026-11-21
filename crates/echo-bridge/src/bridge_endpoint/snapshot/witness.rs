//! T3 camera cases through paired Settings, WebSocket commands and HTTP replies.
use super::{
    command_tests::{assert_reply, command, send},
    voice_tests::{Fixture, fixture_with_camera, outcome},
    *,
};
use crate::bridge_endpoint::snapshot::{CameraSource, Sample};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    pin::Pin,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Notify;

#[path = "journey.rs"]
mod integrated;

// A synthetic 2 x 1 grayscale JPEG.
const JPEG: &str = concat!(
    "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0aHBwgJC4n",
    "ICIsIxwcKDcpLDAxNDQ0Hyc5PTgyPC4zNDL/wAALCAABAAIBAREA/8QAFAABAAAAAAAAAAAAAAAAAAAABv/E",
    "ABQQAQAAAAAAAAAAAAAAAAAAAAD/2gAIAQEAAD8ANP/Z",
);

#[derive(Clone, Copy, Default)]
enum Capture {
    #[default]
    Ready,
    Error,
    Oversize,
    /// A capture time Port OS would refuse: 301 s before now.
    Stale,
    Held,
}

struct CameraPeer {
    shutter: watch::Sender<bool>,
    mode: Mutex<Capture>,
    captures: AtomicUsize,
    entered: Notify,
}

impl CameraPeer {
    fn open() -> Arc<Self> {
        Arc::new(Self {
            shutter: watch::channel(false).0,
            mode: Mutex::default(),
            captures: AtomicUsize::new(0),
            entered: Notify::new(),
        })
    }

    fn mode(&self, mode: Capture) {
        *self.mode.lock().unwrap() = mode;
    }

    async fn started(&self, count: usize) {
        while self.captures.load(Ordering::SeqCst) < count {
            self.entered.notified().await;
        }
        assert_eq!(self.captures.load(Ordering::SeqCst), count);
    }
}

impl CameraSource for CameraPeer {
    fn shutter(&self) -> watch::Receiver<bool> {
        self.shutter.subscribe()
    }

    fn capture(&self) -> Pin<Box<dyn Future<Output = Option<Sample>> + Send + '_>> {
        Box::pin(async move {
            self.captures.fetch_add(1, Ordering::SeqCst);
            self.entered.notify_one();
            let mode = *self.mode.lock().unwrap();
            let jpeg = match mode {
                Capture::Ready | Capture::Stale => STANDARD.decode(JPEG).unwrap(),
                Capture::Error => return None,
                // The base64 fits by itself. Envelope overhead exceeds the cap.
                Capture::Oversize => vec![0; echo_contracts::endpoint::MAX_SNAPSHOT_BYTES + 1],
                Capture::Held => return pending().await,
            };
            Some(Sample {
                jpeg,
                width: 2,
                height: 1,
                sampled_at: if matches!(mode, Capture::Stale) {
                    sampled_at() - Duration::from_secs(301)
                } else {
                    sampled_at()
                },
            })
        })
    }
}

/// Port OS accepts a sample time within 300 s of its own clock, so the fake
/// camera samples at the real wall time, truncated to whole milliseconds.
fn sampled_at() -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(sampled_at_ms().unsigned_abs())
}

fn sampled_at_ms() -> i64 {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    i64::try_from(now.as_millis()).unwrap() / 1000 * 1000 + 125
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_snapshot_without_a_source() {
    Box::pin(bridge_mcp::bridge_component::drive(async {
        let home = tempfile::tempdir().unwrap();
        let mock = Mock::bind_at_epoch(EPOCH).await.unwrap();
        mock.enable_endpoint_audience().unwrap();
        let mut grant = registered(&mock, home.path()).await;
        let mut gateway = Gateway::open(&mock).await;
        let settings = Settings::open(home.path(), &mock, &gateway, grant.grant.clone()).await;
        settings.save(true, "Hall Echo").await;
        gateway.assert_upgrade(CLAIMED).await;
        send(&gateway, &snapshot_request("absent", "BASE64")).await;
        assert_reply(&mock, "absent", "NOT_SUPPORTED").await;
        settings.close().await;
        gateway.assert_disconnect().await;
        grant.close().await;
    }))
    .await;
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_snapshot_obeys_shutter_and_recovers_from_failures() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(bridge_mcp::bridge_component::drive(async {
            let home = tempfile::tempdir().unwrap();
            let mock = Mock::bind_at_epoch(EPOCH).await.unwrap();
            mock.enable_endpoint_audience().unwrap();
            let mut grant = registered(&mock, home.path()).await;
            let mut gateway = Gateway::open(&mock).await;
            let camera = CameraPeer::open();
            let Fixture {
                settings,
                mut device,
                app: _app,
                stale,
            } = fixture_with_camera(home.path(), &mock, &gateway, grant.grant.clone(), Some(camera.clone())).await;
            settings.save(true, "Hall Echo").await;
            gateway.assert_upgrade(CLAIMED).await;
            refusals_before_capture(&gateway, &mock, &camera).await;
            assert_jpeg(&gateway, &mock, "unmuted").await;
            device.send("hassmic-mute", json!({"muted":true})).await;
            stale.notified().await;
            assert_jpeg(&gateway, &mock, "muted").await;
            camera.started(2).await;
            capture_failures(&gateway, &mock, &camera).await;
            assert_snapshot_race(&gateway, &mock, &camera).await;
            settings.close().await;
            gateway.assert_disconnect().await;
            grant.close().await;
        })))
        .await;
}

async fn refusals_before_capture(gateway: &Gateway, mock: &Mock, camera: &CameraPeer) {
    send(gateway, &snapshot_request("invalid", "GIF")).await;
    assert_reply(mock, "invalid", "INVALID_REQUEST").await;
    camera.shutter.send_replace(true);
    send(gateway, &snapshot_request("closed", "BASE64")).await;
    assert_reply(mock, "closed", "SHUTTERED").await;
    assert_eq!(camera.captures.load(Ordering::SeqCst), 0);
    camera.shutter.send_replace(false);
}

async fn capture_failures(gateway: &Gateway, mock: &Mock, camera: &CameraPeer) {
    for (mode, id) in [
        (Capture::Error, "error"),
        (Capture::Oversize, "oversize"),
        (Capture::Stale, "stale"),
    ] {
        camera.mode(mode);
        send(gateway, &snapshot_request(id, "BASE64")).await;
        assert_reply(mock, id, "CAMERA_ERROR").await;
    }
    camera.mode(Capture::Held);
    send(gateway, &snapshot_request("timeout", "BASE64")).await;
    camera.started(6).await;
    send(gateway, &snapshot_request("busy", "BASE64")).await;
    assert_reply(mock, "busy", "CAMERA_BUSY").await;
    advance(Duration::from_secs(5)).await;
    assert_reply(mock, "timeout", "TIMEOUT").await;
}

async fn assert_snapshot_race(gateway: &Gateway, mock: &Mock, camera: &CameraPeer) {
    send(gateway, &snapshot_request("race", "BASE64")).await;
    camera.started(7).await;
    camera.shutter.send_replace(true);
    camera.shutter.send_replace(false);
    assert_reply(mock, "race", "SHUTTERED").await;
    camera.mode(Capture::Ready);
    assert_jpeg(gateway, mock, "recovered").await;
    camera.started(8).await;
}

fn snapshot_request(id: &str, format: &str) -> Value {
    command("get-snapshot", id, DEVICE, &json!({"responseFormat":format}))
}

/// Whatever `responseFormat` asks for (URL, BASE64 or nothing), the reply is
/// the inline base64 JPEG stamped with its capture time.
async fn assert_jpeg(gateway: &Gateway, mock: &Mock, id: &str) {
    let body = match id {
        "unmuted" => json!({"responseFormat":"URL"}),
        "muted" => json!({"responseFormat":"BASE64"}),
        _ => json!({}),
    };
    send(gateway, &command("get-snapshot", id, DEVICE, &body)).await;
    assert_eq!(
        outcome(mock, id).await,
        json!({"snapshot":{"width":2,"height":1,"data":JPEG}})
    );
    let reply = command_tests::receipts(mock)
        .into_iter()
        .filter_map(|request| request.json)
        .find(|body| body["payload"]["requestId"] == id)
        .unwrap();
    let sampled = reply["payload"]["sampledAtMs"].as_i64().unwrap();
    assert_eq!(sampled % 1000, 125);
    assert!((sampled - sampled_at_ms()).abs() < 300_000);
}
