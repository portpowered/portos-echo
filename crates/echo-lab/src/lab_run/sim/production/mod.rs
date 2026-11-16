//! Configured production child and authenticated screen observation for M1.
pub mod report;
pub mod restart;
use std::{net::SocketAddr, path::Path};

use echo_screen_contract::ScreenFrame;
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    process::Command,
};

pub fn selected(bridge: &Path) -> bool {
    bridge.file_stem().is_some_and(|name| name == "echo-bridge")
}

pub fn configure(command: &mut Command, root: &Path, address: SocketAddr, live: &str, mcp: &str) -> Result<(), String> {
    let directory = root.join("configured.private");
    std::fs::create_dir(&directory).map_err(|_| "launch: cannot create private configuration")?;
    let template = include_str!("../../../../../../examples/bridge.example.toml")
        .replace("echo.example.invalid", "127.0.0.1")
        .replace("wyoming_port = 16700", &format!("wyoming_port = {}", address.port()))
        .replace("mode = \"chatgpt\"", "mode = \"api_key\"")
        .replace("voice = \"default\"", "voice = \"alloy\"")
        .replace("0.0.0.0:16900", "127.0.0.1:0") // lint-rules: allow-literal inert example replaced with loopback
        // FakeLive's script completes one turn per call. Use the existing
        // idle policy to give each wake a fresh controlled provider session.
        .replace("session_idle_seconds = 120", "session_idle_seconds = 0")
        .replace("credentials/live.json", "credential")
        .replace("credentials/screen-pairing.txt", "pairing")
        .replace("http://127.0.0.1:18080/mcp", mcp);
    for (name, contents) in [
        ("bridge.toml", template.as_str()),
        ("credential", "synthetic-local-api"),
        ("pairing", "synthetic-local-screen"),
    ] {
        std::fs::write(directory.join(name), contents).map_err(|_| "launch: cannot write private configuration")?;
    }
    for name in [
        "ECHO_LAB_WYOMING_ADDR",
        "ECHO_LAB_LIVE_URL",
        "ECHO_LAB_TRACE_PATH",
        "ECHO_LAB_SESSION_IDLE_SECONDS",
    ] {
        command.env_remove(name);
    }
    let origin = live.trim_end_matches('/');
    command.current_dir(directory).args([
        "--config",
        "bridge.toml",
        "--health-listen",
        "127.0.0.1:0",
        "--live-loopback",
        origin,
        "--model-loopback",
        origin,
    ]);
    command.arg("--trace").arg(root.join("bridge.private.jsonl"));
    Ok(())
}

pub struct Screen {
    socket: TcpStream,
    current: ScreenFrame,
}

impl Screen {
    pub async fn connect(address: SocketAddr, initial: bool) -> Result<Self, String> {
        let mut socket = TcpStream::connect(address)
            .await
            .map_err(|_| "screen: connection failed")?;
        socket
            .write_all(b"GET /events?token=synthetic-local-screen HTTP/1.1\r\nHost: local\r\n\r\n")
            .await
            .map_err(|_| "screen: request failed")?;
        if line(&mut socket).await? != "HTTP/1.1 200 OK\r" {
            return Err("screen: authentication failed".into());
        }
        let current = frame(&mut socket).await?;
        if initial
            && (current.seq != 1
                || current.state
                    != (echo_screen_contract::UiState::Ready {
                        message: "Echo Live ready".into(),
                    }))
        {
            return Err("screen: expected exactly one initial Echo Live ready presentation".into());
        }
        Ok(Self { socket, current })
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
            let next = frame(&mut self.socket).await?;
            if next.seq <= self.current.seq {
                return Err("screen: stale presentation".into());
            }
            self.current = next;
        }
    }
}

async fn frame(socket: &mut TcpStream) -> Result<ScreenFrame, String> {
    loop {
        if let Some(data) = line(socket).await?.strip_prefix("data: ") {
            return serde_json::from_str(data).map_err(|_| "screen: malformed presentation".into());
        }
    }
}

async fn line(socket: &mut TcpStream) -> Result<String, String> {
    let mut bytes = Vec::new();
    loop {
        let byte = socket.read_u8().await.map_err(|_| "screen: stream ended")?;
        if byte == b'\n' {
            return String::from_utf8(bytes).map_err(|_| "screen: invalid UTF-8".into());
        }
        if bytes.len() == 65_536 {
            return Err("screen: oversized line".into());
        }
        bytes.push(byte);
    }
}
