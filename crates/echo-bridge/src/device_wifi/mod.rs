//! Device-role Wi-Fi v1 endpoint and the paired screen's transport adapter.
#[cfg(target_os = "android")]
mod android;
#[cfg(not(target_os = "android"))]
mod fixture;
mod wire;

use echo_core_api::{Clock, SystemClock};
use serde_json::Value;
use std::{io, os::unix::fs::FileTypeExt, path::Path, sync::Mutex, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use wire::{Error, Operation, Reading, Request};

static RADIO: Mutex<()> = Mutex::new(());

pub(crate) fn screen_socket(local_provider: bool) -> Option<std::path::PathBuf> {
    if cfg!(target_os = "android") {
        return Some(echo_device::WIFI_SOCKET.into());
    }
    if local_provider {
        return std::env::var_os("ECHO_LAB_WIFI_SOCKET").map(Into::into);
    }
    None
}

/// Wi-Fi operations and monotonic transport deadline for the device role.
pub const HANDLER: echo_device::WifiHandler = echo_device::WifiHandler {
    respond: handle,
    monotonic: || SystemClock.now().monotonic,
};

fn handle(socket: &Path, bytes: &[u8]) -> Vec<u8> {
    let request = match wire::request(bytes) {
        Ok(request) => request,
        Err((op, error)) => return wire::failure(op, error),
    };
    let op = request.operation();
    let Ok(_radio) = RADIO.try_lock() else {
        let error = match op {
            Operation::Join | Operation::Forget => Error::Busy,
            Operation::Status | Operation::Scan => Error::Unavailable,
        };
        return wire::failure(Some(op), error);
    };
    #[cfg(target_os = "android")]
    let result = android::operate(socket, request);
    #[cfg(not(target_os = "android"))]
    let result = fixture::operate(socket, request);
    result.unwrap_or_else(|error| wire::failure(Some(op), error))
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
    let request = match wire::request(bytes) {
        Ok(request) => request,
        Err((op, Error::Unsupported)) => return Some(wire::failure(op, Error::Unsupported)),
        Err(_) => return None,
    };
    let op = request.operation();
    let Some(path) = path else {
        return Some(wire::failure(Some(op), Error::Unsupported));
    };
    let deadline = if op == Operation::Join { 65 } else { 10 };
    let reply = tokio::time::timeout(Duration::from_secs(deadline), exchange(path, &request)).await;
    Some(match reply {
        Ok(Ok(data)) => data,
        Ok(Err(error)) if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) => {
            wire::failure(Some(op), Error::Unsupported)
        }
        _ => wire::failure(Some(op), Error::Unavailable),
    })
}

async fn exchange(path: &Path, request: &Request) -> io::Result<Vec<u8>> {
    for parent in path.ancestors().skip(1) {
        if !std::fs::symlink_metadata(parent)?.is_dir() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
    }
    if !std::fs::symlink_metadata(path)?.file_type().is_socket() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut socket = tokio::net::UnixStream::connect(path).await?;
    socket.write_all(&wire::encode_request(request)).await?;
    socket.write_all(b"\n").await?;
    let mut bytes = Vec::new();
    BufReader::new(socket)
        .take(wire::RESULT_LIMIT)
        .read_until(b'\n', &mut bytes)
        .await?;
    if bytes.pop() != Some(b'\n') || !valid_result(request, &bytes) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(bytes)
}

fn valid_result(request: &Request, bytes: &[u8]) -> bool {
    let Ok(reply) = serde_json::from_slice::<wire::ResultFrame>(bytes) else {
        return false;
    };
    let data = reply.data;
    if reply.kind != "wifi-result" || data.version != 1 || data.op != request.operation() {
        return false;
    }
    let mut fields = data.fields;
    if !data.ok {
        let error = fields
            .remove("error")
            .and_then(|v| serde_json::from_value::<Error>(v).ok());
        return match (request, error) {
            (_, Some(Error::Unavailable | Error::Unsupported)) => fields.is_empty(),
            (Request::Join { .. } | Request::Forget { .. }, Some(Error::Busy))
            | (Request::Forget { .. }, Some(Error::NotSaved)) => fields.is_empty(),
            (Request::Join { .. }, Some(Error::JoinFailed | Error::RollbackFailed)) => valid_status(fields, None),
            _ => false,
        };
    }
    match request {
        Request::Status | Request::Forget { .. } => valid_status(fields, None),
        Request::Join { ssid, .. } => valid_status(fields, Some(ssid)),
        Request::Scan => {
            matches!(serde_json::from_value::<Reading>(Value::Object(fields)),
                Ok(Reading::Scan { networks }) if networks.iter().all(|net|
                    wire::valid_ssid(&net.ssid) && net.network_id.is_none_or(|id| id >= 0)))
        }
    }
}

fn valid_status(fields: serde_json::Map<String, Value>, target: Option<&str>) -> bool {
    if !fields.contains_key("ssid") {
        return false;
    }
    match serde_json::from_value::<Reading>(Value::Object(fields)) {
        Ok(Reading::Status { ssid, reachable }) => match target {
            Some(target) => reachable && ssid.as_deref() == Some(target),
            None => ssid.as_ref().map_or(!reachable, |name| wire::valid_ssid(name)),
        },
        _ => false,
    }
}
