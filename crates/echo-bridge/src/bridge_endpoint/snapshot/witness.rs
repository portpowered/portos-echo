//! T3 camera cases through paired Settings, WebSocket commands and HTTP replies.
use super::{
    command_tests::{assert_reply, command, send},
    voice_tests::{Fixture, fixture_with_camera, outcome},
    *,
};
use crate::bridge_endpoint::snapshot::{Camera, CameraSource, CaptureError, CaptureFuture, Sample, Shutter};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Notify;

#[path = "http_peer.rs"]
mod http_peer;
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
    PermissionDenied,
    Busy,
    Empty,
    Oversize,
    /// A capture time Port OS would refuse: 301 s before now.
    Stale,
    Held,
}

struct CameraPeer {
    shutter: watch::Sender<Shutter>,
    mode: Mutex<Capture>,
    captures: AtomicUsize,
    entered: Notify,
    exchange: Option<echo_screen_server::Camera>,
}

impl CameraPeer {
    fn open() -> Arc<Self> {
        Arc::new(Self {
            shutter: watch::channel(Shutter::Open).0,
            mode: Mutex::default(),
            captures: AtomicUsize::new(0),
            entered: Notify::new(),
            exchange: None,
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
    fn available(&self) -> bool {
        self.exchange.as_ref().is_none_or(echo_screen_server::Camera::connected)
    }

    fn link(&self) -> Option<watch::Receiver<echo_screen_server::CameraLink>> {
        self.exchange.as_ref().map(echo_screen_server::Camera::link)
    }

    fn shutter(&self) -> watch::Receiver<Shutter> {
        self.shutter.subscribe()
    }

    fn shutter_now(&self) -> Shutter {
        *self.shutter.borrow()
    }

    fn capture(&self, deadline: Instant) -> CaptureFuture<'_> {
        Box::pin(async move {
            self.captures.fetch_add(1, Ordering::SeqCst);
            self.entered.notify_one();
            if let Some(exchange) = &self.exchange {
                let sample = exchange.capture("0", deadline).await?;
                return Ok(Sample {
                    jpeg: sample.jpeg,
                    width: sample.width,
                    height: sample.height,
                    sampled_at: sample.sampled_at,
                });
            }
            let mode = *self.mode.lock().unwrap();
            let jpeg = match mode {
                Capture::Ready | Capture::Stale => STANDARD.decode(JPEG).unwrap(),
                Capture::Error => return Err(CaptureError::Camera),
                Capture::PermissionDenied => return Err(CaptureError::PermissionDenied),
                Capture::Busy => return Err(CaptureError::Busy),
                Capture::Empty => Vec::new(),
                Capture::Oversize => vec![0; echo_contracts::endpoint::MAX_SNAPSHOT_BYTES + 1],
                Capture::Held => return pending().await,
            };
            Ok(Sample {
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

#[allow(clippy::disallowed_methods, reason = "Host time for test")]
fn sampled_at_ms() -> i64 {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    i64::try_from(now.as_millis()).unwrap() / 1000 * 1000 + 125
}

#[tokio::test(start_paused = true)]
async fn snapshot_rechecks_shutter_after_preparing_the_report() {
    let source = CameraPeer::open();
    let mut camera = Camera::default();
    camera.source = Some(source.clone());
    let result = camera
        .reply(json!({}), Instant::now(), None, |snapshot, _| {
            assert_eq!(snapshot.data, JPEG);
            source.shutter.send_replace(Shutter::Closed);
            source.shutter.send_replace(Shutter::Open);
        })
        .await;
    assert_eq!(result.unwrap_err().code(), "SHUTTERED");
    let recovered = camera
        .reply(json!({}), Instant::now(), None, |snapshot, _| snapshot)
        .await
        .unwrap();
    assert_eq!(recovered.data, JPEG);
}

/// A bridge that cannot read the shutter (no `input` group on the Echo) refuses
/// with `CAMERA_ERROR`, never captures, and leaves one fixed-reason trace row.
#[tokio::test(start_paused = true)]
async fn snapshot_traces_an_unreadable_shutter() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("trace.jsonl");
    let trace = crate::bridge_trace::Trace::open(&path, "test").unwrap();
    let source = CameraPeer::open();
    source.shutter.send_replace(Shutter::Unknown);
    let mut camera = Camera::default();
    camera.source = Some(source.clone());
    let result = camera
        .reply(json!({}), Instant::now(), Some(&trace), |snapshot, _| snapshot)
        .await;
    assert_eq!(result.unwrap_err().code(), "CAMERA_ERROR");
    assert_eq!(source.captures.load(Ordering::SeqCst), 0);
    let rows: Vec<Value> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["event"], "endpoint.camera_error");
    assert_eq!(rows[0]["reason"], "shutter input unavailable");
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
            let trace = std::fs::read_to_string(home.path().join("settings-trace")).unwrap();
            assert_eq!(trace.matches("camera permission denied").count(), 1);
            assert_eq!(trace.matches("shutter input unavailable").count(), 1);
            settings.close().await;
            gateway.assert_disconnect().await;
            grant.close().await;
        })))
        .await;
}

async fn refusals_before_capture(gateway: &Gateway, mock: &Mock, camera: &CameraPeer) {
    send(gateway, &snapshot_request("invalid", "GIF")).await;
    assert_reply(mock, "invalid", "INVALID_REQUEST").await;
    camera.shutter.send_replace(Shutter::Closed);
    send(gateway, &snapshot_request("closed", "BASE64")).await;
    assert_reply(mock, "closed", "SHUTTERED").await;
    camera.shutter.send_replace(Shutter::Unknown);
    send(gateway, &snapshot_request("unknown-shutter", "BASE64")).await;
    assert_reply(mock, "unknown-shutter", "CAMERA_ERROR").await;
    assert_eq!(camera.captures.load(Ordering::SeqCst), 0);
    camera.shutter.send_replace(Shutter::Open);
}

async fn capture_failures(gateway: &Gateway, mock: &Mock, camera: &CameraPeer) {
    for (mode, id) in [
        (Capture::Error, "error"),
        (Capture::Empty, "empty"),
        (Capture::Oversize, "oversize"),
        (Capture::Stale, "stale"),
    ] {
        camera.mode(mode);
        send(gateway, &snapshot_request(id, "BASE64")).await;
        assert_reply(mock, id, "CAMERA_ERROR").await;
    }
    camera.mode(Capture::PermissionDenied);
    send(gateway, &snapshot_request("permission", "BASE64")).await;
    assert_reply(mock, "permission", "CAMERA_ERROR").await;
    camera.mode(Capture::Busy);
    send(gateway, &snapshot_request("os-busy", "BASE64")).await;
    assert_reply(mock, "os-busy", "CAMERA_BUSY").await;
    camera.mode(Capture::Held);
    send(gateway, &snapshot_request("timeout", "BASE64")).await;
    camera.started(9).await;
    send(gateway, &snapshot_request("busy", "BASE64")).await;
    assert_reply(mock, "busy", "CAMERA_BUSY").await;
    advance(Duration::from_secs(3)).await;
    assert_reply(mock, "timeout", "TIMEOUT").await;
}

async fn assert_snapshot_race(gateway: &Gateway, mock: &Mock, camera: &CameraPeer) {
    send(gateway, &snapshot_request("race", "BASE64")).await;
    camera.started(10).await;
    camera.shutter.send_replace(Shutter::Closed);
    camera.shutter.send_replace(Shutter::Open);
    assert_reply(mock, "race", "SHUTTERED").await;
    send(gateway, &snapshot_request("lost-shutter", "BASE64")).await;
    camera.started(11).await;
    camera.shutter.send_replace(Shutter::Unknown);
    assert_reply(mock, "lost-shutter", "CAMERA_ERROR").await;
    camera.shutter.send_replace(Shutter::Open);
    camera.mode(Capture::Ready);
    assert_jpeg(gateway, mock, "recovered").await;
    camera.started(12).await;
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
