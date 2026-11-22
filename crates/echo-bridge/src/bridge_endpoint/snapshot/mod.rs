//! A snapshot request owns one cancellable capture and its shutter observation.
use super::commands::{invalid, refusal};
use crate::bridge_trace::Trace;
use base64::{Engine, engine::general_purpose::STANDARD};
pub(super) use echo_camera::Shutter;
use echo_contracts::endpoint::{GetSnapshot, MAX_SNAPSHOT_BYTES, Reply, SnapshotReply};
pub(super) use echo_screen_server::CaptureError;
use echo_screen_server::{CameraLink, CameraRefused};
use serde_json::Value;
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::{
    sync::{Mutex, watch},
    time::{Instant, sleep_until},
};

#[cfg(target_os = "android")]
mod native;
#[cfg(target_os = "android")]
pub(super) use native::NativeCamera;

pub(super) const CAPTURE_WINDOW: Duration = Duration::from_secs(3);
/// Port OS refuses a `sampledAtMs` more than 300 s from its own clock.
const SAMPLE_SKEW: Duration = Duration::from_secs(300);

pub(super) struct Sample {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub sampled_at: SystemTime,
}

pub(super) type CaptureFuture<'a> = Pin<Box<dyn Future<Output = Result<Sample, CaptureError>> + Send + 'a>>;

pub(super) trait CameraSource: Send + Sync {
    fn available(&self) -> bool {
        true
    }

    /// The native app's camera stream, when this source captures through one.
    fn link(&self) -> Option<watch::Receiver<CameraLink>> {
        None
    }

    /// Start a fresh observation and publish its first unsafe transition.
    fn shutter(&self) -> watch::Receiver<Shutter>;

    /// Read the same active hardware observation at the reply commit boundary.
    fn shutter_now(&self) -> Shutter;

    /// Dropping the future cancels capture.
    fn capture(&self, deadline: Instant) -> CaptureFuture<'_>;
}

#[derive(Default)]
pub(super) struct Camera {
    pub(super) source: Option<Arc<dyn CameraSource>>,
    capture: Mutex<()>,
}

impl Camera {
    /// Keep observing the shutter while the caller prepares the inline report.
    pub(super) async fn reply<T>(
        &self,
        body: Value,
        received: Instant,
        trace: Option<&Trace>,
        prepare: impl FnOnce(SnapshotReply, i64) -> T,
    ) -> Result<T, Reply> {
        if !body.is_object() || serde_json::from_value::<GetSnapshot>(body).is_err() {
            return Err(invalid());
        }
        // No camera path is permanent; a stream the app has not attached
        // yet is the snapshot group's retryable refusal.
        let source = self.source.as_ref().ok_or_else(unsupported)?;
        if !source.available() {
            return Err(camera_busy());
        }
        let deadline = received + CAPTURE_WINDOW;
        if Instant::now() >= deadline {
            return Err(timed_out());
        }
        let _capture = self.capture.try_lock().map_err(|_| camera_busy())?;
        let (sample, shutter) = capture(source.as_ref(), deadline, trace).await?;
        let (snapshot, sampled_at_ms) = sample.encode()?;
        let reply = prepare(snapshot, sampled_at_ms);
        check_commit(source.as_ref(), &shutter, deadline, trace)?;
        Ok(reply)
    }
}

async fn capture(
    source: &dyn CameraSource,
    deadline: Instant,
    trace: Option<&Trace>,
) -> Result<(Sample, watch::Receiver<Shutter>), Reply> {
    let mut shutter = source.shutter();
    shutter_open(*shutter.borrow_and_update(), trace)?;
    check_commit(source, &shutter, deadline, trace)?;
    let sample = tokio::select! {
        biased;
        changed = shutter.changed() => {
            return Err(match (changed, *shutter.borrow()) {
                (Ok(()), Shutter::Closed | Shutter::Open) => shuttered(),
                _ => camera_error(),
            });
        }
        () = sleep_until(deadline) => return Err(timed_out()),
        result = source.capture(deadline) => result.map_err(|error| capture_failure(error, trace))?,
    };
    Ok((sample, shutter))
}

fn check_commit(
    source: &dyn CameraSource,
    shutter: &watch::Receiver<Shutter>,
    deadline: Instant,
    trace: Option<&Trace>,
) -> Result<(), Reply> {
    shutter_open(source.shutter_now(), trace)?;
    match shutter.has_changed() {
        Ok(false) => {}
        Ok(true) if *shutter.borrow() != Shutter::Unknown => return Err(shuttered()),
        _ => return Err(camera_error()),
    }
    if Instant::now() >= deadline {
        return Err(timed_out());
    }
    Ok(())
}

/// An unreadable shutter is `CAMERA_ERROR`, never a capture. On the Echo it
/// usually means the bridge cannot open the shutter's evdev node (it needs the
/// `input` group, ADR 0033), so the refusal leaves a fixed-reason trace row.
fn shutter_open(state: Shutter, trace: Option<&Trace>) -> Result<(), Reply> {
    match state {
        Shutter::Open => Ok(()),
        Shutter::Closed => Err(shuttered()),
        Shutter::Unknown => {
            super::note(
                trace,
                "endpoint.camera_error",
                serde_json::json!({"reason": "shutter input unavailable"}),
            );
            Err(camera_error())
        }
    }
}

fn capture_failure(error: CaptureError, trace: Option<&Trace>) -> Reply {
    match error {
        // A stream that detached mid-capture is retryable like contention.
        CaptureError::Unavailable | CaptureError::Busy => camera_busy(),
        CaptureError::PermissionDenied => {
            super::note(
                trace,
                "endpoint.camera_error",
                serde_json::json!({"reason": "camera permission denied"}),
            );
            camera_error()
        }
        CaptureError::Camera => camera_error(),
        CaptureError::Timeout => timed_out(),
    }
}

impl Sample {
    fn encode(self) -> Result<(SnapshotReply, i64), Reply> {
        if !(1..=1920).contains(&self.width)
            || !(1..=1080).contains(&self.height)
            || self.jpeg.is_empty()
            || self.jpeg.len() > MAX_SNAPSHOT_BYTES
        {
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

fn unsupported() -> Reply {
    refusal("NOT_SUPPORTED")
}

fn camera_busy() -> Reply {
    refusal("CAMERA_BUSY")
}

/// Trace the native camera stream from its first state: attach, detach and the
/// fixed reason of the latest refused attach. Never a nonce, key or address.
pub(super) async fn observe(source: Option<Arc<dyn CameraSource>>, trace: Option<Trace>) {
    if let Some(mut link) = source.and_then(|source| source.link()) {
        loop {
            let now = *link.borrow_and_update();
            let refused = now.refused.map(|reason| match reason {
                CameraRefused::Forbidden => "forbidden",
                CameraRefused::Unauthorized => "unauthorized",
                CameraRefused::Conflict => "conflict",
            });
            let session = if now.attached { "attached" } else { "detached" };
            let fields = serde_json::json!({ "session": session, "refused": refused });
            super::note(trace.as_ref(), "endpoint.camera", fields);
            if link.changed().await.is_err() {
                break;
            }
        }
    }
    std::future::pending::<()>().await;
}
