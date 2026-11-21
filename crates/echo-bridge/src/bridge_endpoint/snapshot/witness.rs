//! T3 camera cases through paired Settings, WebSocket commands and HTTP replies.
use super::{
    command_tests::{assert_reply, command, send},
    voice_tests::{Fixture, fixture_with_camera, outcome},
    *,
};
use crate::bridge_endpoint::snapshot::{CameraSource, Sample};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::UNIX_EPOCH,
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
                Capture::Ready => STANDARD.decode(JPEG).unwrap(),
                Capture::Error => return None,
                // The base64 fits by itself. Envelope overhead exceeds the cap.
                Capture::Oversize => vec![0; transport::MAX_MESSAGE / 4 * 3 - 3],
                Capture::Held => return pending().await,
            };
            Some(Sample {
                jpeg,
                width: 2,
                height: 1,
                sampled_at: UNIX_EPOCH + Duration::from_secs(EPOCH) + Duration::from_millis(125),
            })
        })
    }
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_snapshot_without_a_source() {
    Box::pin(bridge_mcp::bridge_component::drive(async {
        let home = tempfile::tempdir().unwrap();
        let mock = Mock::bind_at_epoch(EPOCH).await.unwrap();
        mock.enable_endpoint_audience().unwrap();
        let mut grant = registered(&mock, home.path()).await;
        let mut gateway = Gateway::open().await;
        let settings = Settings::open(home.path(), &mock, &gateway, grant.grant.clone()).await;
        settings.save(true, "Hall Echo").await;
        gateway.assert_upgrade(4).await;
        send(&gateway, &snapshot_request("absent", "BASE64")).await;
        assert_reply(
            &mock,
            "absent",
            "get-snapshot",
            "NOT_SUPPORTED",
            "The camera is unavailable.",
        )
        .await;
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
            let mut gateway = Gateway::open().await;
            let camera = CameraPeer::open();
            let Fixture {
                settings,
                mut device,
                app: _app,
                stale,
            } = fixture_with_camera(home.path(), &mock, &gateway, grant.grant.clone(), Some(camera.clone())).await;
            settings.save(true, "Hall Echo").await;
            gateway.assert_upgrade(4).await;
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
    send(gateway, &snapshot_request("invalid", "URL")).await;
    assert_reply(
        mock,
        "invalid",
        "get-snapshot",
        "INVALID_REQUEST",
        "The command payload is invalid.",
    )
    .await;
    camera.shutter.send_replace(true);
    send(gateway, &snapshot_request("closed", "BASE64")).await;
    assert_reply(
        mock,
        "closed",
        "get-snapshot",
        "SHUTTERED",
        "The camera shutter is closed.",
    )
    .await;
    assert_eq!(camera.captures.load(Ordering::SeqCst), 0);
    camera.shutter.send_replace(false);
}

async fn capture_failures(gateway: &Gateway, mock: &Mock, camera: &CameraPeer) {
    for (mode, id) in [(Capture::Error, "error"), (Capture::Oversize, "oversize")] {
        camera.mode(mode);
        send(gateway, &snapshot_request(id, "BASE64")).await;
        assert_reply(
            mock,
            id,
            "get-snapshot",
            "CAMERA_ERROR",
            "The camera could not provide a snapshot.",
        )
        .await;
    }
    camera.mode(Capture::Held);
    send(gateway, &snapshot_request("timeout", "BASE64")).await;
    camera.started(5).await;
    send(gateway, &snapshot_request("busy", "BASE64")).await;
    assert_reply(mock, "busy", "get-snapshot", "CAMERA_BUSY", "The camera is busy.").await;
    advance(Duration::from_secs(5)).await;
    assert_reply(
        mock,
        "timeout",
        "get-snapshot",
        "TIMEOUT",
        "The camera capture timed out.",
    )
    .await;
}

async fn assert_snapshot_race(gateway: &Gateway, mock: &Mock, camera: &CameraPeer) {
    send(gateway, &snapshot_request("race", "BASE64")).await;
    camera.started(6).await;
    camera.shutter.send_replace(true);
    camera.shutter.send_replace(false);
    assert_reply(
        mock,
        "race",
        "get-snapshot",
        "SHUTTERED",
        "The camera shutter is closed.",
    )
    .await;
    camera.mode(Capture::Ready);
    assert_jpeg(gateway, mock, "recovered").await;
    camera.started(7).await;
}

fn snapshot_request(id: &str, format: &str) -> Value {
    let mut request = command("get-snapshot", id, ROUTE, &json!({"responseFormat":format}));
    request["header"]["namespace"] = json!("snapshot");
    request
}

async fn assert_jpeg(gateway: &Gateway, mock: &Mock, id: &str) {
    send(gateway, &snapshot_request(id, "BASE64")).await;
    assert_eq!(
        outcome(mock, id).await,
        json!({
            "ok":true,"image":{"mimeType":"image/jpeg","data":JPEG,"width":2,"height":1},
            "timeOfSample":"2027-01-15T08:00:00.125000000Z"
        })
    );
}
