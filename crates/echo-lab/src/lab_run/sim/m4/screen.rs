//! Paired screen observations from the real bridge SSE boundary.
use serde_json::Value;
use std::path::Path;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

pub struct Screen(TcpStream);

impl Screen {
    pub async fn connect(root: &Path) -> Result<Self, String> {
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
        socket
            .write_all(b"GET /events?token=test-pair HTTP/1.1\r\nHost: local\r\n\r\n")
            .await
            .map_err(|_| "screen: request failed")?;
        let mut screen = Self(socket);
        if screen.line().await? != "HTTP/1.1 200 OK" {
            return Err("screen: paired SSE unauthorized".into());
        }
        Ok(screen)
    }

    async fn line(&mut self) -> Result<String, String> {
        let mut bytes = Vec::new();
        loop {
            let byte = self.0.read_u8().await.map_err(|_| "screen: EOF before observation")?;
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
        for _ in 0..128 {
            if let Some(data) = self.line().await?.strip_prefix("data: ") {
                let frame: Value = serde_json::from_str(data).map_err(|_| "screen: malformed state")?;
                return Ok(frame);
            }
        }
        Err("screen: missing state within frame limit".into())
    }
}

#[cfg(test)]
mod tests;
