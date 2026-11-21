use echo_contracts::secret::Secret;
use echo_screen_contract::{ScreenFrame, UiState};
use echo_screen_server::{ScreenServer, serve};
use std::{error::Error, net::SocketAddr};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    sync::mpsc,
};

pub type Result<T = ()> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
pub const TOKEN: &str = "synthetic-screen-pair";

pub async fn server() -> Result<(ScreenServer, mpsc::Sender<UiState>)> {
    let (tx, rx) = mpsc::channel(32);
    Ok((serve("127.0.0.1:0".parse()?, Secret::new(TOKEN.into()), rx).await?, tx))
}

pub async fn connect(
    addr: SocketAddr,
    method: &str,
    path: &str,
    body: &str,
    headers: &str,
) -> Result<BufReader<TcpStream>> {
    let mut socket = TcpStream::connect(addr).await?;
    socket.write_all(format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}", body.len()).as_bytes()).await?;
    Ok(BufReader::new(socket))
}

pub async fn head(stream: &mut BufReader<TcpStream>) -> Result<String> {
    let mut output = String::new();
    loop {
        let mut line = String::new();
        if stream.read_line(&mut line).await? == 0 {
            return Err("missing HTTP headers".into());
        }
        output.push_str(&line);
        if line == "\r\n" {
            return Ok(output);
        }
    }
}

pub async fn request(addr: SocketAddr, method: &str, path: &str, body: &str, headers: &str) -> Result<(u16, String)> {
    let mut socket = connect(addr, method, path, body, headers).await?;
    let head = head(&mut socket).await?;
    assert!(head.contains("cache-control: no-store\r\n"));
    assert!(head.contains("referrer-policy: no-referrer\r\n"));
    assert!(!head.contains("access-control-allow-origin"));
    let code = head.split_whitespace().nth(1).ok_or("missing status")?.parse()?;
    let mut body = String::new();
    socket.read_to_string(&mut body).await?;
    Ok((code, body))
}

pub async fn events(addr: SocketAddr) -> Result<BufReader<TcpStream>> {
    let mut stream = connect(addr, "GET", &format!("/events?token={TOKEN}"), "", "").await?;
    assert!(head(&mut stream).await?.starts_with("HTTP/1.1 200"));
    Ok(stream)
}

pub async fn frame(stream: &mut BufReader<TcpStream>) -> Result<ScreenFrame> {
    loop {
        let mut line = String::new();
        if stream.read_line(&mut line).await? == 0 {
            return Err("unexpected stream EOF".into());
        }
        if let Some(wire) = line.strip_prefix("data: ") {
            return Ok(serde_json::from_str(wire.trim())?);
        }
    }
}

pub async fn post(addr: SocketAddr, body: &str) -> Result<(u16, String)> {
    request(
        addr,
        "POST",
        &format!("/actions?token={TOKEN}"),
        body,
        "Content-Type: application/json\r\n",
    )
    .await
}

pub fn report(message: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stdout(), "{message}");
}

/// Splits a served document into its page build identity and the document without the echo-build meta.
pub fn page_build(html: &str) -> Result<(String, String)> {
    let open = "<head>\n    <meta name=\"echo-build\" content=\"";
    let (before, rest) = html.split_once(open).ok_or("missing page build")?;
    let (build, after) = rest.split_once("\" />").ok_or("unterminated page build")?;
    Ok((build.to_owned(), format!("{before}<head>{after}")))
}
