//! A snapshot request owns one cancellable capture and its shutter observation.
use super::{
    commands::{invalid, refusal},
    transport::MAX_MESSAGE,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use echo_contracts::endpoint::{CommandReply, GetSnapshot, Snapshot, SnapshotImage, Uplink};
use serde::Serialize;
use serde_json::Value;
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::{
    sync::{Mutex, watch},
    time::{Instant, timeout_at},
};

const CAPTURE_WINDOW: Duration = Duration::from_secs(5);

pub(super) struct Sample {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub sampled_at: SystemTime,
}

pub(super) trait CameraSource: Send + Sync {
    /// True while closed. Publish each physical transition, including during capture.
    fn shutter(&self) -> watch::Receiver<bool>;

    /// Dropping the future cancels capture. The source returns only in-memory JPEG.
    fn capture(&self) -> Pin<Box<dyn Future<Output = Option<Sample>> + Send + '_>>;
}

#[derive(Default)]
pub(super) struct Camera {
    pub(super) source: Option<Arc<dyn CameraSource>>,
    capture: Mutex<()>,
}

#[derive(Serialize)]
struct Success {
    ok: bool,
    #[serde(flatten)]
    snapshot: Snapshot,
}

impl Camera {
    pub(super) async fn reply(
        &self,
        route: &str,
        request_id: &str,
        payload: Value,
        received: Instant,
    ) -> Result<Vec<u8>, Value> {
        if !payload.is_object() || serde_json::from_value::<GetSnapshot>(payload).is_err() {
            return Err(invalid());
        }
        let source = self
            .source
            .as_ref()
            .ok_or_else(|| refusal("NOT_SUPPORTED", "The camera is unavailable."))?;
        let mut shutter = source.shutter();
        if *shutter.borrow_and_update() || shutter.has_changed().is_err() {
            return Err(shuttered());
        }
        if Instant::now() >= received + CAPTURE_WINDOW {
            return Err(timed_out());
        }
        let _capture = self
            .capture
            .try_lock()
            .map_err(|_| refusal("CAMERA_BUSY", "The camera is busy."))?;
        let sample = tokio::select! {
            biased;
            _ = shutter.changed() => return Err(shuttered()),
            result = timeout_at(received + CAPTURE_WINDOW, source.capture()) => {
                result.map_err(|_| timed_out())?.ok_or_else(camera_error)?
            }
        };
        let outcome = Success {
            ok: true,
            snapshot: sample.encode()?,
        };
        let reply = CommandReply { request_id, outcome };
        let body = serde_json::to_vec(&Uplink::new("get-snapshot", route, reply)).map_err(|_| camera_error())?;
        if !matches!(shutter.has_changed(), Ok(false)) {
            return Err(shuttered());
        }
        if body.len() > MAX_MESSAGE {
            return Err(camera_error());
        }
        Ok(body)
    }
}

impl Sample {
    fn encode(self) -> Result<Snapshot, Value> {
        if self.width == 0 || self.height == 0 || self.jpeg.is_empty() || self.jpeg.len() > MAX_MESSAGE / 4 * 3 {
            return Err(camera_error());
        }
        Ok(Snapshot {
            image: SnapshotImage::jpeg(STANDARD.encode(self.jpeg), self.width, self.height),
            time_of_sample: sample_time(self.sampled_at)?,
        })
    }
}

fn sample_time(at: SystemTime) -> Result<String, Value> {
    let mut timestamp = crate::bridge_voice::endpoint_timestamp(at).map_err(|_| camera_error())?;
    let nanos = match at.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(offset) => offset.subsec_nanos(),
        Err(offset) => (1_000_000_000 - offset.duration().subsec_nanos()) % 1_000_000_000,
    };
    if nanos != 0 {
        timestamp.pop();
        timestamp.push_str(&format!(".{nanos:09}Z"));
    }
    Ok(timestamp)
}

fn shuttered() -> Value {
    refusal("SHUTTERED", "The camera shutter is closed.")
}

fn timed_out() -> Value {
    refusal("TIMEOUT", "The camera capture timed out.")
}

fn camera_error() -> Value {
    refusal("CAMERA_ERROR", "The camera could not provide a snapshot.")
}
