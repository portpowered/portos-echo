//! Paired control of the existing grant owner, through an owned loopback forward.
use super::{Remote, device_path};
use crate::lab_run::{McpAction, device::runtime::Runtime};
use echo_contracts::secret::Secret;
use serde_json::{Value, json};
use std::net::{Ipv4Addr, SocketAddr};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

const CONTROL: &str = "device: invalid MCP operator response";
const PAIRING: &str = "device: invalid MCP operator pairing";

/// Own the socket directly: cancellation/drop closes the pending request rather
/// than detaching a task that could later mutate the grant.
pub(in crate::lab_run::device) struct Pending {
    socket: TcpStream,
    outcome: &'static str,
}

impl Pending {
    async fn begin(address: SocketAddr, token: &Secret<String>, operation: &str) -> Result<Self, String> {
        let outcome = match operation {
            "link" => "linked",
            "refresh" => "refreshed",
            "unlink" => "unlinked",
            _ => return Err(CONTROL.into()),
        };
        if address.ip() != Ipv4Addr::LOCALHOST || address.port() == 0 {
            return Err(CONTROL.into());
        }
        let mut socket = TcpStream::connect(address).await.map_err(|_| CONTROL)?;
        let body = json!({"operation":operation}).to_string();
        let request = Secret::new(format!(
            "POST /operator/mcp HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            token.expose(),
            body.len()
        ));
        socket
            .write_all(request.expose().as_bytes())
            .await
            .map_err(|_| CONTROL)?;
        Ok(Self { socket, outcome })
    }

    async fn complete(&mut self) -> Result<(), String> {
        let mut bytes = Vec::new();
        (&mut self.socket)
            .take(8193)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| CONTROL)?;
        response(&bytes, self.outcome)
    }
}

fn response(bytes: &[u8], outcome: &str) -> Result<(), String> {
    if bytes.len() > 8192 {
        return Err(CONTROL.into());
    }
    let end = bytes.windows(4).position(|part| part == b"\r\n\r\n").ok_or(CONTROL)?;
    let header = std::str::from_utf8(&bytes[..end]).map_err(|_| CONTROL)?;
    let (status, length, content_type) = headers(header)?;
    if !matches!(status, "HTTP/1.1 200 OK" | "HTTP/1.1 422 Unprocessable Entity") {
        return Err(CONTROL.into());
    }
    let body = &bytes[end + 4..];
    if length != Some(body.len()) || body.len() > 4096 || content_type != Some("application/json") {
        return Err(CONTROL.into());
    }
    let value: Value = serde_json::from_slice(body).map_err(|_| CONTROL)?;
    if status == "HTTP/1.1 200 OK" && value == json!({"outcome":outcome}) {
        return Ok(());
    }
    if status == "HTTP/1.1 422 Unprocessable Entity"
        && value.as_object().is_some_and(|object| object.len() == 1)
        && let Some(category) = value["error"].as_str().filter(|category| {
            matches!(
                *category,
                "discovery" | "transport" | "protocol" | "access_denied" | "expired" | "relink_required" | "storage"
            )
        })
    {
        return Err(format!("device: MCP operator {category}"));
    }
    Err(CONTROL.into())
}

fn headers(header: &str) -> Result<(&str, Option<usize>, Option<&str>), String> {
    let mut lines = header.split("\r\n");
    let status = lines.next().ok_or(CONTROL)?;
    let mut length = None;
    let mut content_type = None;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(CONTROL)?;
        match name.to_ascii_lowercase().as_str() {
            "content-length" => {
                if length
                    .replace(value.trim().parse::<usize>().map_err(|_| CONTROL)?)
                    .is_some()
                {
                    return Err(CONTROL.into());
                }
            }
            "content-type" => {
                if content_type.replace(value.trim()).is_some() {
                    return Err(CONTROL.into());
                }
            }
            "transfer-encoding" => return Err(CONTROL.into()),
            _ => (),
        }
    }
    Ok((status, length, content_type))
}

impl Remote {
    fn pairing(&self, path: &std::path::Path) -> Result<Secret<String>, String> {
        let path = path.to_str().filter(|path| device_path(path)).ok_or(PAIRING)?;
        let bytes = self.read(&format!("head -c 16385 {path}")).map_err(|_| PAIRING)?;
        if bytes.len() > 16384 {
            return Err(PAIRING.into());
        }
        let text = String::from_utf8(bytes).map_err(|_| PAIRING)?;
        let token = text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix('\n'))
            .unwrap_or(&text);
        if token.trim().is_empty() || token.chars().any(char::is_control) {
            return Err(PAIRING.into());
        }
        Ok(Secret::new(token.into()))
    }
}

impl Runtime {
    pub(in crate::lab_run::device) async fn mcp_operator(
        &mut self,
        action: &McpAction,
        config: &str,
    ) -> Result<(), String> {
        if self.portos.is_none() {
            return Err("device: M5 Port OS observer not configured".into());
        }
        if matches!(action, McpAction::Approve) {
            let result = self
                .pending_mcp
                .as_mut()
                .ok_or("device: no pending MCP link")?
                .complete()
                .await;
            self.pending_mcp.take();
            return result;
        }
        if self.pending_mcp.is_some() {
            return Err("device: MCP link already pending".into());
        }
        let operation = match action {
            McpAction::Link => "link",
            McpAction::Refresh => "refresh",
            McpAction::Unlink => "unlink",
            _ => return Err("device: unsupported MCP operator action".into()),
        };
        let (address, token) = self.mcp_control(config)?;
        let mut pending = Pending::begin(address, &token, operation).await?;
        if matches!(action, McpAction::Link) {
            self.pending_mcp = Some(pending);
            Ok(())
        } else {
            pending.complete().await
        }
    }

    fn mcp_control(&mut self, config: &str) -> Result<(SocketAddr, Secret<String>), String> {
        let config = echo_config::BridgeConfig::from_toml_str(config).map_err(|_| PAIRING)?;
        self.observe()?;
        let addresses: Vec<_> = self
            .trace
            .records
            .iter()
            .filter(|record| record["event"] == "mcp.operator.listening")
            .collect();
        let [record] = addresses.as_slice() else {
            return Err(CONTROL.into());
        };
        let address: SocketAddr = record["address"]
            .as_str()
            .ok_or(CONTROL)?
            .parse()
            .map_err(|_| CONTROL)?;
        if address.ip() != Ipv4Addr::LOCALHOST || address.port() == 0 {
            return Err(CONTROL.into());
        }
        let remote = self.remote.as_mut().ok_or("device: MCP requires on-device bridge")?;
        let token = remote.pairing(&config.screen.pairing_token_file)?;
        let address = remote.reach(address)?;
        Ok((address, token))
    }
}

#[cfg(test)]
#[path = "operator_tests.rs"]
mod tests;
