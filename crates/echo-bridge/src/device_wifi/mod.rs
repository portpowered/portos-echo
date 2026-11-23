//! Device-role Wi-Fi v1 endpoint and the paired screen's transport adapter.
#[cfg(target_os = "android")]
mod android;
mod wire;

use echo_core_api::{Clock, SystemClock};
use serde_json::Value;
use std::{io, os::unix::fs::FileTypeExt, path::Path, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use wire::{Error, Operation, Reading};

/// Wi-Fi operations and monotonic transport deadline for the device role.
pub const HANDLER: echo_device::WifiHandler = echo_device::WifiHandler {
    respond: handle,
    monotonic: || SystemClock.now().monotonic,
};

fn handle(socket: &Path, bytes: &[u8]) -> Vec<u8> {
    let op = match wire::request(bytes) {
        Ok(op) => op,
        Err((op, error)) => return wire::failure(op, error),
    };
    match platform(socket, op) {
        Ok(radio) => wire::success(op, radio.project()),
        Err(error) => wire::failure(Some(op), error),
    }
}

#[cfg(target_os = "android")]
fn platform(socket: &Path, op: Operation) -> Result<Reading, Error> {
    android::observe(socket, op).map_err(|_| Error::Unavailable)
}

#[cfg(not(target_os = "android"))]
fn platform(_socket: &Path, op: Operation) -> Result<Reading, Error> {
    use std::{fs::File, io::Read};
    let path = std::env::var_os("ECHO_LAB_WIFI_FIXTURE").ok_or(Error::Unsupported)?;
    let file = File::open(path).map_err(|_| Error::Unavailable)?;
    let fixture: Fixture = serde_json::from_reader(file.take(wire::RESULT_LIMIT)).map_err(|_| Error::Unavailable)?;
    match op {
        Operation::Status => fixture.status,
        Operation::Scan => fixture.scan,
    }
    .ok_or(Error::Unavailable)
}

#[cfg(not(target_os = "android"))]
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    status: Option<Reading>,
    scan: Option<Reading>,
}

/// Relay Wi-Fi v1 between the paired screen server and the device endpoint.
#[must_use]
pub fn relay(path: Option<&Path>) -> echo_screen_server::WifiRelay {
    let path = path.map(Path::to_owned);
    echo_screen_server::WifiRelay::new(move |bytes| {
        let path = path.clone();
        async move { relay_request(path.as_deref(), &bytes).await }
    })
}

async fn relay_request(path: Option<&Path>, bytes: &[u8]) -> Option<Vec<u8>> {
    let op = match wire::request(bytes) {
        Ok(op) => op,
        Err((op, Error::Unsupported)) => return Some(wire::failure(op, Error::Unsupported)),
        Err(_) => return None,
    };
    let Some(path) = path else {
        return Some(wire::failure(Some(op), Error::Unsupported));
    };
    let reply = tokio::time::timeout(Duration::from_secs(10), exchange(path, op)).await;
    Some(match reply {
        Ok(Ok(data)) => data,
        Ok(Err(error)) if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) => {
            wire::failure(Some(op), Error::Unsupported)
        }
        _ => wire::failure(Some(op), Error::Unavailable),
    })
}

async fn exchange(path: &Path, op: Operation) -> io::Result<Vec<u8>> {
    for parent in path.ancestors().skip(1) {
        if !std::fs::symlink_metadata(parent)?.is_dir() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
    }
    if !std::fs::symlink_metadata(path)?.file_type().is_socket() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut socket = tokio::net::UnixStream::connect(path).await?;
    socket.write_all(&wire::encode_request(op)).await?;
    socket.write_all(b"\n").await?;
    let mut bytes = Vec::new();
    BufReader::new(socket)
        .take(wire::RESULT_LIMIT)
        .read_until(b'\n', &mut bytes)
        .await?;
    if bytes.pop() != Some(b'\n') || !valid_result(op, &bytes) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(bytes)
}

fn valid_result(op: Operation, bytes: &[u8]) -> bool {
    // Accept only the expected observation or one of the fixed errors.
    let Ok(reply) = serde_json::from_slice::<wire::ResultFrame>(bytes) else {
        return false;
    };
    let data = reply.data;
    if reply.kind != "wifi-result" || data.version != 1 || data.op != op {
        return false;
    }
    if !data.ok {
        return data.fields.len() == 1
            && matches!(
                data.fields.get("error").and_then(Value::as_str),
                Some("wifi-unavailable" | "wifi-unsupported")
            );
    }
    let has_ssid = data.fields.contains_key("ssid");
    match (op, serde_json::from_value::<Reading>(Value::Object(data.fields))) {
        (Operation::Status, Ok(Reading::Status { ssid, reachable })) => {
            has_ssid && ssid.as_ref().map_or(!reachable, |name| wire::valid_ssid(name))
        }
        (Operation::Scan, Ok(Reading::Scan { networks })) => networks.iter().all(|net| wire::valid_ssid(&net.ssid)),
        _ => false,
    }
}
