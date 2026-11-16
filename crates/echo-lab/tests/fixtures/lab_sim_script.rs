//! Controlled T3 bridge at the runner's public child-process boundary.
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use echo_wyoming::{ClientOptions, WyomingClient};
use serde_json::{Value, json};

fn env(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    Ok(std::env::var(name)?)
}

async fn endpoints() -> Result<WyomingClient, Box<dyn std::error::Error>> {
    let address: SocketAddr = env("ECHO_LAB_WYOMING_ADDR")?.parse()?;
    if address.ip() != Ipv4Addr::LOCALHOST {
        return Err("invalid target".into());
    }
    let client = WyomingClient::connect(address, ClientOptions::default()).await?;
    client.run_satellite().await?;
    client.describe().await?;
    let mut addresses = vec![address];
    for name in ["ECHO_LAB_MCP_URL", "ECHO_LAB_LIVE_URL"] {
        let url = env(name)?;
        let addr: SocketAddr = url
            .strip_prefix("http://")
            .ok_or("invalid URL")?
            .split('/')
            .next()
            .ok_or("missing address")?
            .parse()?;
        if addr.ip() != Ipv4Addr::LOCALHOST {
            return Err("invalid target".into());
        }
        let stream = tokio::net::TcpStream::connect(addr).await?;
        addresses.push(stream.peer_addr()?);
    }
    std::fs::write(env("ECHO_LAB_SIM_ENDPOINTS")?, serde_json::to_vec(&addresses)?)?;
    Ok(client)
}

fn trace(path: &PathBuf, records: &Value, append: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .append(append)
        .create_new(!append)
        .open(path)?;
    for record in records.as_array().ok_or("expected records")? {
        // Deliberately fragment each JSON line; the consumer must handle writes.
        let text = format!("{record}\n");
        let (a, b) = text.as_bytes().split_at(text.len() / 2);
        file.write_all(a)?;
        file.flush()?;
        file.write_all(b)?;
        file.flush()?;
    }
    Ok(())
}

// Test coordinator holds all three real owners live until both worlds have
// announced their endpoints. No endpoint is compared after it can be reused.
async fn ready_barrier(script: &Value) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    if let Some(address) = script["ready_barrier"].as_str() {
        let address: SocketAddr = address.parse()?;
        if address.ip() != Ipv4Addr::LOCALHOST {
            return Err("invalid coordinator target".into());
        }
        let mut control = tokio::net::TcpStream::connect(address).await?;
        let mut endpoints = std::fs::read(env("ECHO_LAB_SIM_ENDPOINTS")?)?;
        endpoints.push(b'\n');
        control.write_all(&endpoints).await?;
        if control.read_u8().await? != b'G' {
            return Err("invalid coordinator release".into());
        }
    }
    Ok(())
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let script: Value = serde_json::from_slice(&std::fs::read(env("ECHO_LAB_SIM_SCRIPT")?)?)?;
    let client = endpoints().await?;
    let path = PathBuf::from(env("ECHO_LAB_TRACE_PATH")?);
    if script["missing_trace"] != true {
        trace(&path, &script["records"], false)?;
    }
    if let Some(ui) = script.get("ui") {
        std::fs::write(env("ECHO_LAB_UI_STATE_PATH")?, serde_json::to_vec(ui)?)?;
    }
    if script["block_report"] == true {
        std::fs::create_dir(path.parent().ok_or("missing parent")?.join("report.pending"))?;
    }
    ready_barrier(&script).await?;
    if script["no_ready"] != true {
        writeln!(io::stdout(), "{}", json!({"event":"bridge.ready"}))?;
    }
    if script["completed"] == true {
        writeln!(
            io::stdout(),
            "{}",
            json!({"event":"bridge.run_completed","run_id":"one"})
        )?;
    }
    io::stdout().flush()?;
    if script["cancel"] == true {
        std::fs::write(env("ECHO_LAB_SIM_CANCEL")?, b"cancel")?;
    }
    let (send, receive) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let result = io::stdin().read(&mut [0]);
        let _ = send.send(result);
    });
    receive.await??;
    if let Some(records) = script.get("late") {
        trace(&path, records, true)?;
    }
    if let Some(raw) = script["raw_tail"].as_str() {
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)?
            .write_all(raw.as_bytes())?;
    }
    client.close().await?;
    if script["fail_close"] == true {
        return Err("controlled close failure".into());
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })
        .and_then(|r| r.block_on(run()));
    std::process::ExitCode::from(u8::from(result.is_err()))
}
