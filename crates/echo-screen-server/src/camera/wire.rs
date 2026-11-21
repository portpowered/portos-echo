use super::{CaptureError, Event, Sample};
use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    response::sse::Event as Frame,
};
use hmac::{Hmac, KeyInit as _, Mac as _};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::time::{Duration, SystemTime};
use tokio::time::Instant;

pub(super) const NONCE: &str = "x-echo-camera-nonce";
pub(super) const PROOF: &str = "x-echo-camera-proof";
const WIDTH: &str = "x-echo-camera-width";
const HEIGHT: &str = "x-echo-camera-height";
const SAMPLED_AT: &str = "x-echo-camera-sampled-at-ms";
const DOMAIN: &str = "echo-screen-camera/v1";

pub(super) fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let all = headers.get_all(name);
    let mut values = all.iter();
    let value = values.next()?.to_str().ok()?;
    values.next().is_none().then_some(value)
}

pub(super) fn opaque_id(text: &str) -> bool {
    lower_hex(text, 32)
}

fn lower_hex(text: &str, size: usize) -> bool {
    text.len() == size
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn mac(key: &[u8; 32], direction: &str, nonce: &str, fields: &[&str]) -> Option<Hmac<Sha256>> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).ok()?;
    mac.update(DOMAIN.as_bytes());
    for value in [direction, nonce].into_iter().chain(fields.iter().copied()) {
        mac.update(b"\n");
        mac.update(value.as_bytes());
    }
    Some(mac)
}

pub(super) fn proof(headers: &HeaderMap) -> Option<[u8; 32]> {
    let proof = header(headers, PROOF)?;
    if !lower_hex(proof, 64) {
        return None;
    }
    let mut bytes = [0_u8; 32];
    hex::decode_to_slice(proof, &mut bytes).ok()?;
    Some(bytes)
}

pub(super) fn verify(key: &[u8; 32], nonce: &str, proof: &[u8; 32], fields: &[&str]) -> bool {
    mac(key, "app", nonce, fields).is_some_and(|mac| mac.verify_slice(proof).is_ok())
}

pub(super) fn frame(key: &[u8; 32], nonce: &str, event: Option<Event>) -> Option<Frame> {
    let (name, data) = match event {
        None => ("ready", "{}".to_owned()),
        Some(Event::Cancel(id)) => ("cancel", serde_json::json!({ "captureId": id }).to_string()),
        Some(Event::Capture {
            id,
            camera_id,
            deadline,
        }) => {
            let remaining = deadline.checked_duration_since(Instant::now())?.as_millis().min(3000);
            if remaining == 0 {
                return None;
            }
            (
                "capture",
                serde_json::json!({
                    "captureId": id, "cameraId": camera_id, "remainingMs": remaining,
                })
                .to_string(),
            )
        }
    };
    let proof = hex::encode(
        mac(key, "bridge", nonce, &["event", name, &data])?
            .finalize()
            .into_bytes(),
    );
    Some(Frame::default().event(name).id(proof).data(data))
}

pub(super) struct Metadata<'a> {
    content_type: &'a str,
    width: &'a str,
    height: &'a str,
    sampled_at: &'a str,
}

impl<'a> Metadata<'a> {
    pub(super) fn read(headers: &'a HeaderMap) -> Result<Self, StatusCode> {
        let content_type = header(headers, "content-type").ok_or(StatusCode::UNSUPPORTED_MEDIA_TYPE)?;
        let mut metadata = Self {
            content_type,
            width: "",
            height: "",
            sampled_at: "",
        };
        match content_type {
            "image/jpeg" => {
                metadata.width = header(headers, WIDTH).ok_or(StatusCode::BAD_REQUEST)?;
                metadata.height = header(headers, HEIGHT).ok_or(StatusCode::BAD_REQUEST)?;
                metadata.sampled_at = header(headers, SAMPLED_AT).ok_or(StatusCode::BAD_REQUEST)?;
                Ok(metadata)
            }
            "application/json"
                if ![WIDTH, HEIGHT, SAMPLED_AT]
                    .iter()
                    .any(|name| headers.contains_key(*name)) =>
            {
                Ok(metadata)
            }
            "application/json" => Err(StatusCode::BAD_REQUEST),
            _ => Err(StatusCode::UNSUPPORTED_MEDIA_TYPE),
        }
    }

    pub(super) fn verify(&self, key: &[u8; 32], nonce: &str, proof: &[u8; 32], id: &str, bytes: &[u8]) -> bool {
        let digest = hex::encode(Sha256::digest(bytes));
        verify(
            key,
            nonce,
            proof,
            &[
                "result",
                id,
                self.content_type,
                self.width,
                self.height,
                self.sampled_at,
                &digest,
            ],
        )
    }

    pub(super) fn result(&self, bytes: Bytes) -> Option<Result<Sample, CaptureError>> {
        if self.content_type == "application/json" {
            return Some(Err(serde_json::from_slice::<Failure>(&bytes).ok()?.error.into()));
        }
        let width = decimal::<u32>(self.width).filter(|value| (1..=1920).contains(value))?;
        let height = decimal::<u32>(self.height).filter(|value| (1..=1080).contains(value))?;
        let millis = decimal::<i64>(self.sampled_at).and_then(|ms| u64::try_from(ms).ok())?;
        let sampled_at = SystemTime::UNIX_EPOCH.checked_add(Duration::from_millis(millis))?;
        Some(Ok(Sample {
            jpeg: bytes.into(),
            width,
            height,
            sampled_at,
        }))
    }
}

fn decimal<T: std::str::FromStr + ToString>(text: &str) -> Option<T> {
    let value: T = text.parse().ok()?;
    (value.to_string() == text).then_some(value)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Failure {
    error: ErrorCode,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ErrorCode {
    Busy,
    PermissionDenied,
    CameraError,
    Timeout,
}

impl From<ErrorCode> for CaptureError {
    fn from(error: ErrorCode) -> Self {
        match error {
            ErrorCode::Busy => Self::Busy,
            ErrorCode::PermissionDenied => Self::PermissionDenied,
            ErrorCode::CameraError => Self::Camera,
            ErrorCode::Timeout => Self::Timeout,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_wire_matches_shared_mac_vectors() {
        let key = Sha256::digest(b"synthetic-screen-pair").into();
        let nonce = "0123456789abcdef0123456789abcdef";
        for line in include_str!("../../../../tests/echo-screen/camera-vectors.tsv").lines() {
            let mut fields = line.split('\t');
            let expected = fields.next().unwrap();
            let direction = fields.next().unwrap();
            let signed: Vec<_> = fields.collect();
            let calculated = mac(&key, direction, nonce, &signed).unwrap().finalize().into_bytes();
            assert_eq!(hex::encode(calculated), expected);
        }
    }

    #[test]
    fn camera_wire_rejects_noncanonical_dimensions_or_strict_error_variants() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "image/jpeg".parse().unwrap());
        headers.insert(HEIGHT, "1080".parse().unwrap());
        headers.insert(SAMPLED_AT, "125".parse().unwrap());
        for rejected in ["0", "01920", "1921", "+10"] {
            headers.insert(WIDTH, rejected.parse().unwrap());
            assert!(
                Metadata::read(&headers)
                    .unwrap()
                    .result(Bytes::from_static(b"synthetic"))
                    .is_none()
            );
        }
        headers.insert(WIDTH, "1920".parse().unwrap());
        let accepted = Metadata::read(&headers)
            .unwrap()
            .result(Bytes::from_static(b"synthetic"))
            .unwrap()
            .unwrap();
        assert_eq!((accepted.width, accepted.height), (1920, 1080));
        assert_eq!(accepted.jpeg, b"synthetic");
        headers.clear();
        headers.insert("content-type", "application/json".parse().unwrap());
        let parser = Metadata::read(&headers).unwrap();
        assert!(matches!(
            parser.result(Bytes::from_static(br#"{"error":"busy"}"#)),
            Some(Err(CaptureError::Busy))
        ));
        for rejected in [
            br#"{"error":"busy","extra":true}"#.as_slice(),
            br#"{"error":"busy","error":"timeout"}"#.as_slice(),
            br#"{"error":"unavailable"}"#.as_slice(),
        ] {
            assert!(parser.result(Bytes::from_static(rejected)).is_none());
        }
    }
}
