//! Configured production child and authenticated screen observation for M1.
pub mod mute;
pub mod report;
pub mod restart;
pub mod wifi;
use std::{net::SocketAddr, path::Path};

use echo_screen_contract::ScreenFrame;
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

pub fn selected(bridge: &Path) -> bool {
    bridge.file_stem().is_some_and(|name| name == "echo-bridge")
}

mod config;
pub use config::{configure, configure_codex};

pub struct Screen {
    socket: TcpStream,
    current: ScreenFrame,
    pending: Vec<u8>,
}

impl Screen {
    pub fn current(&self) -> Result<Value, String> {
        serde_json::to_value(&self.current).map_err(|_| "screen: invalid frame".into())
    }

    pub async fn next(&mut self) -> Result<Value, String> {
        self.current = frame(&mut self.socket, &mut self.pending).await?;
        self.current()
    }
    pub async fn connect(address: SocketAddr, initial: bool) -> Result<Self, String> {
        let mut socket = TcpStream::connect(address)
            .await
            .map_err(|_| "screen: connection failed")?;
        socket
            .write_all(b"GET /events?token=synthetic-local-screen HTTP/1.1\r\nHost: local\r\n\r\n")
            .await
            .map_err(|_| "screen: request failed")?;
        let mut pending = Vec::new();
        if line(&mut socket, &mut pending).await? != "HTTP/1.1 200 OK\r" {
            return Err("screen: authentication failed".into());
        }
        let current = frame(&mut socket, &mut pending).await?;
        if initial
            && (current.seq != 1
                || current.state
                    != (echo_screen_contract::UiState::Ready {
                        message: "Echo Live ready".into(),
                    }))
        {
            return Err("screen: expected exactly one initial Echo Live ready presentation".into());
        }
        Ok(Self {
            socket,
            current,
            pending,
        })
    }

    pub fn sequence(&self) -> u64 {
        self.current.seq
    }

    pub fn muted(&self) -> bool {
        self.current.state == echo_screen_contract::UiState::Muted
    }

    pub async fn expect(&mut self, expected: &Value) -> Result<(), String> {
        loop {
            let bytes = serde_json::to_vec(&self.current.state).map_err(|_| "screen: invalid state")?;
            if super::observers::screen(&bytes, expected).is_ok() {
                return Ok(());
            }
            let next = frame(&mut self.socket, &mut self.pending).await?;
            if next.seq <= self.current.seq {
                return Err("screen: stale presentation".into());
            }
            self.current = next;
        }
    }
}

async fn frame(socket: &mut TcpStream, pending: &mut Vec<u8>) -> Result<ScreenFrame, String> {
    loop {
        if let Some(data) = line(socket, pending).await?.strip_prefix("data: ") {
            return serde_json::from_str(data).map_err(|_| "screen: malformed presentation".into());
        }
    }
}

async fn line(socket: &mut TcpStream, bytes: &mut Vec<u8>) -> Result<String, String> {
    loop {
        let byte = socket.read_u8().await.map_err(|_| {
            if bytes.is_empty() {
                "screen: stream ended"
            } else {
                "screen: incomplete line"
            }
        })?;
        if byte == b'\n' {
            return String::from_utf8(std::mem::take(bytes)).map_err(|_| "screen: invalid UTF-8".into());
        }
        if bytes.len() == 65_536 {
            return Err("screen: oversized line".into());
        }
        bytes.push(byte);
    }
}

/// Every observed call must use the nominated dummy key and local V3 route.
/// A wake is counted independently from the production bridge trace.
pub(super) fn valid_provider(snapshot: &Value, trace: &super::trace::Trace, history: &[super::trace::Trace]) -> bool {
    let wakes = history
        .iter()
        .chain(std::iter::once(trace))
        .flat_map(|t| &t.records)
        .filter(|row| row["event"] == "wyoming.run_ended")
        .count();
    snapshot
        == &serde_json::json!({"event":"provider.snapshot", "calls":wakes, "matching_calls":wakes,
        "sidebands":wakes, "external":0, "refreshes":0})
}
