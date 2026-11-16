use super::state::Service;
use crate::handler_output;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::Shutdown,
    os::unix::net::UnixStream as BlockingStream,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

pub(super) const INPUT_MAX: usize = 65536;
pub(super) const OUTPUT_MAX: usize = 1_048_576;
pub(super) const IO_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u8,
    output: Option<Accepted>,
    error: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Accepted {
    accepted: bool,
    task_id: String,
}

/// Execute the stdin handler with an independent total-lifetime watchdog.
/// # Errors
/// Returns only the contract's bounded transport/core rejection text.
pub fn run_handler() -> Result<(), String> {
    let socket = std::env::var_os("ECHO_MUSE_REPORT_SOCKET").ok_or("report socket is not configured")?;
    if !std::path::Path::new(&socket).is_absolute() {
        return Err("report socket is not configured".into());
    }
    let reading = Arc::new(AtomicBool::new(true));
    let state = reading.clone();
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    // The main thread's watchdog includes blocked stdin and stdout. Returning from
    // the executable terminates a stuck worker; no runtime shutdown waits for stdin.
    std::thread::spawn(move || {
        let _sent = send.send(handler_worker(&socket, &state));
    });
    receive.recv_timeout(Duration::from_millis(7900)).map_err(|_| {
        if reading.load(Ordering::SeqCst) {
            "report input timed out".to_string()
        } else {
            "report transport timed out".to_string()
        }
    })?
}

fn handler_worker(socket: &std::ffi::OsStr, reading: &AtomicBool) -> Result<(), String> {
    let mut input = Vec::new();
    std::io::stdin()
        .take((INPUT_MAX + 1) as u64)
        .read_to_end(&mut input)
        .map_err(|_| "report transport unavailable")?;
    reading.store(false, Ordering::SeqCst);
    if input.len() > INPUT_MAX {
        return Err("report input too large".into());
    }
    let mut stream =
        BlockingStream::connect(std::path::Path::new(socket)).map_err(|_| "report transport unavailable")?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|error| transport_error(&error))?;
    stream
        .set_write_timeout(Some(IO_TIMEOUT))
        .map_err(|error| transport_error(&error))?;
    let size = u32::try_from(input.len()).map_err(|_| "report input too large")?;
    stream
        .write_all(&size.to_be_bytes())
        .map_err(|error| transport_error(&error))?;
    stream.write_all(&input).map_err(|error| transport_error(&error))?;
    stream
        .shutdown(Shutdown::Write)
        .map_err(|error| transport_error(&error))?;
    let mut header = [0; 4];
    stream
        .read_exact(&mut header)
        .map_err(|error| transport_error(&error))?;
    let size = u32::from_be_bytes(header) as usize;
    if size > OUTPUT_MAX {
        return Err("report response too large".into());
    }
    if size == 0 {
        return Err("invalid report response".into());
    }
    let mut response = vec![0; size];
    stream
        .read_exact(&mut response)
        .map_err(|error| transport_error(&error))?;
    let mut trailing = [0];
    if stream.read(&mut trailing).map_err(|error| transport_error(&error))? != 0 {
        return Err("invalid report response".into());
    }
    let output = parse_response(&input, &response)?;
    std::io::stdout()
        .write_all(output.as_bytes())
        .map_err(|_| "report transport unavailable".into())
}

fn transport_error(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => "report transport timed out",
        _ => "report transport unavailable",
    }
    .into()
}

pub(super) fn parse_response(input: &[u8], bytes: &[u8]) -> Result<String, String> {
    let invalid = || "invalid report response".to_string();
    let parsed: Envelope = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let envelope = value.as_object().ok_or_else(invalid)?;
    if envelope.len() != 2 || parsed.version != 1 {
        return Err(invalid());
    }
    if let Some(error) = parsed.error {
        if error.is_empty() {
            return Err(invalid());
        }
        return Err(error.replace(['\r', '\n'], " "));
    }
    let output = parsed.output.ok_or_else(invalid)?;
    let request: Value = serde_json::from_slice(input).map_err(|_| invalid())?;
    if !output.accepted || request["task_id"].as_str() != Some(&output.task_id) {
        return Err(invalid());
    }
    Ok(json!({"accepted":true,"task_id":output.task_id}).to_string())
}

async fn read_frame(stream: &mut UnixStream) -> Result<String, ()> {
    let size = stream.read_u32().await.map_err(|_| ())? as usize;
    if !(1..=INPUT_MAX).contains(&size) {
        return Err(());
    }
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).await.map_err(|_| ())?;
    let mut trailing = [0];
    if stream.read(&mut trailing).await.map_err(|_| ())? != 0 {
        return Err(());
    }
    String::from_utf8(bytes).map_err(|_| ())
}

pub(super) async fn serve(mut stream: UnixStream, service: Arc<Service>) {
    let input = tokio::time::timeout(IO_TIMEOUT, read_frame(&mut stream)).await;
    let response = match input {
        Err(_) => return,
        Ok(Err(())) => json!({"version":1,"error":"invalid report frame"}),
        Ok(Ok(_)) if service.stopping.load(Ordering::SeqCst) => {
            json!({"version":1,"error":"report service shutting down"})
        }
        Ok(Ok(input)) => {
            let (stdout, stderr, exit) = handler_output(service.backend.report_intake().accept(&input));
            if exit == 0 {
                match serde_json::from_str::<Value>(&stdout) {
                    Ok(output) => json!({"version":1,"output":output}),
                    Err(_) => return,
                }
            } else {
                json!({"version":1,"error":stderr})
            }
        }
    }
    .to_string();
    let Ok(size) = u32::try_from(response.len()) else {
        return;
    };
    let write = async {
        stream.write_all(&size.to_be_bytes()).await?;
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await
    };
    let _written = tokio::time::timeout(IO_TIMEOUT, write).await;
}
