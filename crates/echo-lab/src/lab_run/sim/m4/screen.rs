//! Paired screen observations from the real bridge SSE boundary.
use serde_json::Value;
use std::path::Path;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

pub mod lips;

const PLAIN: &str = "/events?token=test-pair";
const LIPS: &str = "/events?token=test-pair&mouth=1";

pub struct Screen {
    socket: TcpStream,
    /// The name of the event whose data line comes next, when it has one.
    named: Option<String>,
    /// Every lip-sync batch seen so far, in order.
    pub mouth: Vec<Value>,
}

impl Screen {
    pub async fn connect(root: &Path) -> Result<Self, String> {
        Self::open(root, PLAIN).await
    }

    /// As `connect`, also asking the screen server for lip-sync batches.
    pub async fn lips(root: &Path) -> Result<Self, String> {
        Self::open(root, LIPS).await
    }

    async fn open(root: &Path, path: &str) -> Result<Self, String> {
        let raw = std::fs::read_to_string(root.join("trace")).map_err(|_| "screen: missing trace")?;
        let address = raw
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|value| value["event"] == "screen.listening")
            .and_then(|value| value["address"].as_str().map(str::to_owned))
            .ok_or("screen: missing listener")?;
        let address: std::net::SocketAddr = address.parse().map_err(|_| "screen: invalid address")?;
        if address.ip() != std::net::Ipv4Addr::LOCALHOST || address.port() == 0 {
            return Err("screen: non-loopback listener".into());
        }
        let mut socket = TcpStream::connect(address)
            .await
            .map_err(|_| "screen: connection failed")?;
        // Request an EOF-delimited body so SSE completeness excludes HTTP chunk framing.
        socket
            .write_all(format!("GET {path} HTTP/1.0\r\nHost: local\r\nConnection: close\r\n\r\n").as_bytes())
            .await
            .map_err(|_| "screen: request failed")?;
        let mut screen = Self {
            socket,
            named: None,
            mouth: vec![],
        };
        if !matches!(screen.line().await?.as_str(), "HTTP/1.0 200 OK" | "HTTP/1.1 200 OK") {
            return Err("screen: paired SSE unauthorized".into());
        }
        Ok(screen)
    }

    async fn line(&mut self) -> Result<String, String> {
        let mut bytes = Vec::new();
        loop {
            let byte = self
                .socket
                .read_u8()
                .await
                .map_err(|_| "screen: EOF before observation")?;
            if byte == b'\n' {
                break;
            }
            if bytes.len() == 65536 {
                return Err("screen: oversized frame".into());
            }
            bytes.push(byte);
        }
        String::from_utf8(bytes)
            .map(|line| line.trim_end_matches('\r').to_owned())
            .map_err(|_| "screen: malformed frame".into())
    }

    pub async fn expect(&mut self, expected: &str) -> Result<Value, String> {
        let frame = self.next().await?;
        if frame["state"]["state"] != expected {
            return Err(format!(
                "screen: expected current state {expected}, observed {}",
                frame["state"]["state"]
            ));
        }
        Ok(frame["state"].clone())
    }

    pub async fn next(&mut self) -> Result<Value, String> {
        for _ in 0..1024 {
            let line = self.line().await?;
            if let Some(frame) = self.take(&line)? {
                return Ok(frame);
            }
        }
        Err("screen: missing state within frame limit".into())
    }

    /// One SSE line: an unnamed data line is a screen frame; a named `mouth`
    /// one is kept apart, and consumes no frame.
    fn take(&mut self, line: &str) -> Result<Option<Value>, String> {
        if let Some(name) = line.strip_prefix("event: ") {
            self.named = Some(name.to_owned());
            return Ok(None);
        }
        let Some(data) = line.strip_prefix("data: ") else {
            return Ok(None);
        };
        let value: Value = serde_json::from_str(data).map_err(|_| "screen: malformed state")?;
        match self.named.take().as_deref() {
            None => return Ok(Some(value)),
            Some("mouth") => self.mouth.push(value),
            Some(_) => {}
        }
        Ok(None)
    }

    /// After the bridge is collected, retain every remaining publication up to EOF.
    pub async fn remaining(&mut self) -> Result<Vec<Value>, String> {
        let mut bytes = Vec::new();
        (&mut self.socket)
            .take(65537)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "screen: final collection failed")?;
        if bytes.len() > 65536 || (!bytes.is_empty() && bytes != b"\n" && !bytes.ends_with(b"\n\n")) {
            return Err("screen: incomplete or oversized final publication".into());
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| "screen: malformed final publication")?;
        let mut frames = vec![];
        for line in text.lines() {
            frames.extend(self.take(line)?);
        }
        Ok(frames)
    }
}

#[cfg(test)]
mod tests;
