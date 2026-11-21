//! A snapshot request owns one cancellable capture and its shutter observation.
use super::commands::{invalid, refusal};
use base64::{Engine, engine::general_purpose::STANDARD};
use echo_contracts::endpoint::{GetSnapshot, MAX_SNAPSHOT_BYTES, Reply, SnapshotReply};
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
/// Port OS refuses a `sampledAtMs` more than 300 s from its own clock.
const SAMPLE_SKEW: Duration = Duration::from_secs(300);

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

impl Camera {
    /// Capture one JPEG for a `get-snapshot` command. Refused only while the
    /// shutter is closed, or when the camera cannot provide one. The reply is
    /// always inline base64, whatever `responseFormat` asks for, and carries
    /// the capture time.
    pub(super) async fn reply(&self, body: Value, received: Instant) -> Result<(SnapshotReply, i64), Reply> {
        if !body.is_object() || serde_json::from_value::<GetSnapshot>(body).is_err() {
            return Err(invalid());
        }
        let source = self.source.as_ref().ok_or_else(|| refusal("NOT_SUPPORTED"))?;
        let mut shutter = source.shutter();
        if *shutter.borrow_and_update() || shutter.has_changed().is_err() {
            return Err(shuttered());
        }
        if Instant::now() >= received + CAPTURE_WINDOW {
            return Err(timed_out());
        }
        let _capture = self.capture.try_lock().map_err(|_| refusal("CAMERA_BUSY"))?;
        let sample = tokio::select! {
            biased;
            _ = shutter.changed() => return Err(shuttered()),
            result = timeout_at(received + CAPTURE_WINDOW, source.capture()) => {
                result.map_err(|_| timed_out())?.ok_or_else(camera_error)?
            }
        };
        let reply = sample.encode()?;
        if !matches!(shutter.has_changed(), Ok(false)) {
            return Err(shuttered());
        }
        Ok(reply)
    }
}

impl Sample {
    fn encode(self) -> Result<(SnapshotReply, i64), Reply> {
        if self.width == 0 || self.height == 0 || self.jpeg.is_empty() || self.jpeg.len() > MAX_SNAPSHOT_BYTES {
            return Err(camera_error());
        }
        let now = crate::bridge_time::now();
        let skew = now
            .duration_since(self.sampled_at)
            .or_else(|_| self.sampled_at.duration_since(now))
            .map_err(|_| camera_error())?;
        if skew > SAMPLE_SKEW {
            return Err(camera_error());
        }
        let sampled_at_ms = self
            .sampled_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .and_then(|since| i64::try_from(since.as_millis()).ok())
            .ok_or_else(camera_error)?;
        let reply = SnapshotReply {
            width: self.width,
            height: self.height,
            data: STANDARD.encode(self.jpeg),
        };
        Ok((reply, sampled_at_ms))
    }
}

fn shuttered() -> Reply {
    refusal("SHUTTERED")
}

fn timed_out() -> Reply {
    refusal("TIMEOUT")
}

fn camera_error() -> Reply {
    refusal("CAMERA_ERROR")
}
