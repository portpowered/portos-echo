use super::mcp_lights::Journey;
use std::path::Path;

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    net::TcpStream,
    process::Command,
};

pub type Screen = Lines<BufReader<TcpStream>>;
const PAIR: &str = "synthetic-m5-screen";

pub fn command(binary: &Path, root: &Path) -> Command {
    let mut command = Command::new(binary);
    command.current_dir(root).kill_on_drop(true);
    for name in [
        "ECHO_LAB_WYOMING_ADDR",
        "ECHO_LAB_MCP_URL",
        "ECHO_LAB_LIVE_URL",
        "ECHO_LAB_TRACE_PATH",
        "ECHO_LAB_UI_STATE_PATH",
        "ECHO_LAB_SESSION_IDLE_SECONDS",
    ] {
        command.env_remove(name);
    }
    command
}

pub async fn screen(address: &str) -> Result<Screen, String> {
    let mut socket = TcpStream::connect(address)
        .await
        .map_err(|_| "M5: screen connection failed")?;
    socket
        .write_all(format!("GET /events?token={PAIR} HTTP/1.1\r\nHost: local\r\n\r\n").as_bytes())
        .await
        .map_err(|_| "M5: screen request failed")?;
    let mut lines = BufReader::new(socket).lines();
    if lines
        .next_line()
        .await
        .map_err(|_| "M5: screen response failed")?
        .as_deref()
        != Some("HTTP/1.1 200 OK")
    {
        return Err("M5: screen pairing refused".into());
    }
    Ok(lines)
}

pub async fn state(lines: &mut Screen) -> Result<Value, String> {
    loop {
        let line = lines
            .next_line()
            .await
            .map_err(|_| "M5: screen stream failed")?
            .ok_or("M5: screen stream closed")?;
        if line.len() > 65536 {
            return Err("M5: oversized screen event".into());
        }
        if let Some(data) = line.strip_prefix("data: ") {
            let value: Value = serde_json::from_str(data).map_err(|_| "M5: malformed screen event")?;
            return Ok(value["state"].clone());
        }
    }
}

pub fn configure(root: &Path, host: std::net::SocketAddr, resource: &str, journey: Journey) -> Result<(), String> {
    let template = include_str!("../../../../../../examples/bridge.example.toml");
    let text = template
        .replace("echo.example.invalid", "127.0.0.1")
        .replace("wyoming_port = 16700", &format!("wyoming_port = {}", host.port()))
        .replace("mode = \"chatgpt\"", "mode = 'api_key'")
        .replace("credentials/live.json", "credential")
        .replace("voice = \"default\"\n", "")
        .replace("[agent]", "[agent]\nmodel = 'gpt-5.4'")
        .replace(
            "[[mcp.server]]",
            "[mcp]\nstate_dir = 'portos'\nclient_id = 'echo-lab'\noperator_listen = '127.0.0.1:0'\n[[mcp.server]]",
        )
        .replace("http://127.0.0.1:18080/mcp", resource)
        .replace("auth = \"none\"", "auth = 'portos_device_code'")
        .replace(&format!("{}:16900", std::net::Ipv4Addr::UNSPECIFIED), "127.0.0.1:0");
    let mut text = match journey {
        Journey::Lifecycle => text,
        Journey::Twin | Journey::ReadOnly => text.replace(
            "allow_tools = [\"test.echo\"]",
            "allow_tools = ['control', 'endpoint_query', 'group_query']",
        ),
    };
    if journey == Journey::ReadOnly {
        text = text.replace("[mcp]\n", "[mcp]\nscopes = ['endpoint:read']\n");
    }
    let text = format!("{text}\n[codex]\nhome = '{}'\n", root.join("home").display());
    std::fs::create_dir(root.join("credentials")).map_err(|_| "M5: cannot create pairing directory")?;
    for (name, content) in [
        ("bridge.toml", text.as_str()),
        ("credential", "synthetic-api-key\n"),
        ("credentials/screen-pairing.txt", PAIR),
    ] {
        std::fs::write(root.join(name), content).map_err(|_| "M5: cannot write private configuration")?;
    }
    Ok(())
}

pub fn completion(output: &std::process::Output, expected: &str) -> Result<(), String> {
    if !output.status.success() || output.stdout != expected.as_bytes() || !output.stderr.is_empty() {
        return Err("M5: operator completion mismatch".into());
    }
    Ok(())
}

pub fn link_command(
    binary: &Path,
    root: &Path,
) -> (
    tokio::task::JoinHandle<std::io::Result<std::process::Output>>,
    tokio::sync::oneshot::Receiver<Vec<u8>>,
) {
    let mut command = command(binary, root);
    command.args(["link", "--config", "bridge.toml"]);
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let (send, receive) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut child = command.spawn()?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("missing link stdout"))?;
        let mut prefix = Vec::new();
        while prefix.iter().filter(|byte| **byte == b'\n').count() != 2 {
            if prefix.len() == 4096 {
                return Err(std::io::Error::other("oversized link guidance"));
            }
            prefix.push(stdout.read_u8().await?);
        }
        send.send(prefix.clone())
            .map_err(|_| std::io::Error::other("link observer closed"))?;
        child.stdout = Some(stdout);
        let mut output = child.wait_with_output().await?;
        prefix.extend_from_slice(&output.stdout);
        output.stdout = prefix;
        Ok(output)
    });
    (task, receive)
}

pub fn link_code(epoch: u64, issuer: &str) -> Value {
    json!({"state":"link_code","user_code":"TEST-CODE",
        "verification_uri":format!("{issuer}/approve"),"expires_at_unix":epoch + 1800})
}

pub(super) fn endpoint(value: &str) -> Result<std::net::SocketAddr, String> {
    value
        .strip_prefix("http://")
        .and_then(|value| value.split('/').next())
        .ok_or_else(|| "M5: invalid local endpoint".into())
        .and_then(|value| value.parse().map_err(|_| "M5: invalid local endpoint".into()))
}
