//! Fake bridge calls the actual owned loopback MCP listener once.
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpStream,
};

pub(super) fn probe(contents: &str, fixture: &Value) -> Result<(), Box<dyn std::error::Error>> {
    let config = echo_config::BridgeConfig::from_toml_str(contents)?;
    let server = config
        .mcp
        .server
        .iter()
        .find(|server| server.allow_tools.iter().any(|tool| tool == "test.echo"))
        .ok_or("missing spoof endpoint")?;
    let url = url::Url::parse(&server.url)?;
    if url.host_str() != Some("127.0.0.1") {
        return Err("fake refuses nonlocal endpoint".into());
    }
    let mut socket = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, url.port().ok_or("missing port")?))?;
    socket.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    let body = json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"test.echo","arguments":{"text":"synthetic setup witness"}}})
    .to_string();
    write!(
        socket,
        "POST /mcp HTTP/1.1\r\nHost: local\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )?;
    let mut response = String::new();
    socket.read_to_string(&mut response)?;
    let (status, body) = response.split_once("\r\n\r\n").ok_or("missing HTTP response")?;
    if !status.starts_with("HTTP/1.1 200 OK") {
        return Err("spoof HTTP failure".into());
    }
    let response: Value = serde_json::from_str(body)?;
    let path = std::path::Path::new(fixture["launch_record"].as_str().ok_or("missing record")?)
        .with_file_name("spoof-response.json");
    std::fs::write(path, response.to_string())?;
    Ok(())
}
