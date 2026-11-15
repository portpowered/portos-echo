#![cfg(test)]
use echo_config::{McpAuth, McpServer};
use echo_contracts::secret::Secret;
use echo_mcp_client::{McpClient, TokenError, TokenProvider};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct Provider(pub Arc<Mutex<String>>);
impl TokenProvider for Provider {
    fn token(&self) -> impl Future<Output = Result<Secret<String>, TokenError>> {
        std::future::ready(Ok(Secret::new(self.0.lock().unwrap().clone())))
    }
}
pub fn host(url: &str, value: Arc<Mutex<String>>) -> McpClient<Provider> {
    McpClient::new(
        McpServer {
            name: "peer".into(),
            url: url.into(),
            auth: McpAuth::None,
            bearer_file: None,
            allow_tools: vec!["test.echo".into()],
        },
        Provider(value),
        Duration::from_secs(3),
    )
    .unwrap()
}
#[derive(Clone)]
pub struct Observation {
    pub request: Value,
    pub bearer_equal: bool,
    pub post: bool,
    pub forbidden_headers: [bool; 2],
}
pub enum Reply {
    Result(Value),
    Sse(Value),
    Chunked(&'static str, Vec<u8>),
    Raw(u16, &'static str, Vec<u8>),
    Error(i64, String),
}
pub struct Peer {
    pub url: String,
    pub observations: Arc<Mutex<Vec<Observation>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Peer {
    pub async fn start(replies: Vec<Reply>, token: Arc<Mutex<String>>) -> Self {
        Self::with_notification(replies, token, None).await
    }
    pub async fn with_notification(
        replies: Vec<Reply>,
        token: Arc<Mutex<String>>,
        mut notification: Option<Reply>,
    ) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let observations = Arc::new(Mutex::new(Vec::new()));
        let records = observations.clone();
        let task = tokio::spawn(async move {
            let mut replies: VecDeque<_> = replies.into();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let (head, request) = read(&mut stream).await;
                let expected = format!("authorization: Bearer {}", token.lock().unwrap());
                records.lock().unwrap().push(Observation {
                    request: request.clone(),
                    post: head.starts_with("POST /mcp "),
                    bearer_equal: head.lines().any(|line| line == expected),
                    forbidden_headers: [
                        head.to_lowercase().contains("mcp-session-id:"),
                        head.to_lowercase().contains("dpop:"),
                    ],
                });
                let reply = match request["method"].as_str() {
                    Some("initialize") => Reply::Result(
                        json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"peer","version":"1"}}),
                    ),
                    Some("notifications/initialized") => notification
                        .take()
                        .unwrap_or_else(|| Reply::Raw(202, "application/json", vec![])),
                    _ => replies.pop_front().unwrap(),
                };
                let _ = write(&mut stream, reply, &request["id"]).await;
            }
        });
        Self {
            url,
            observations,
            task,
        }
    }
    pub async fn shutdown(self) {
        self.task.abort();
        assert!(self.task.await.unwrap_err().is_cancelled());
    }
}
async fn read(stream: &mut tokio::net::TcpStream) -> (String, Value) {
    let mut bytes = Vec::new();
    let mut byte = [0; 1];
    while !bytes.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await.unwrap();
        bytes.push(byte[0]);
    }
    let head = String::from_utf8(bytes).unwrap();
    let length: usize = head
        .lines()
        .find_map(|line| {
            line.to_lowercase()
                .strip_prefix("content-length:")
                .map(|s| s.trim().parse().unwrap())
        })
        .unwrap();
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.unwrap();
    (head, serde_json::from_slice(&body).unwrap())
}
async fn write(stream: &mut tokio::net::TcpStream, reply: Reply, id: &Value) -> std::io::Result<()> {
    let (status, media, body, sse) = match reply {
        Reply::Result(result) => (
            200,
            "application/json",
            serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"result":result})).unwrap(),
            false,
        ),
        Reply::Error(code, message) => (
            200,
            "application/json",
            serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})).unwrap(),
            false,
        ),
        Reply::Sse(result) => (
            200,
            "text/event-stream",
            format!("data: {}\n\n", json!({"jsonrpc":"2.0","id":id,"result":result})).into_bytes(),
            true,
        ),
        Reply::Chunked(media, body) => (200, media, body, true),
        Reply::Raw(status, media, body) => (status, media, body, false),
    };
    let header = if sse {
        format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: {media}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        )
    } else {
        format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: {media}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
    };
    stream.write_all(header.as_bytes()).await?;
    if sse {
        let size = if media == "text/event-stream" { 3 } else { 65_536 };
        for chunk in body.chunks(size) {
            stream.write_all(format!("{:x}\r\n", chunk.len()).as_bytes()).await?;
            stream.write_all(chunk).await?;
            stream.write_all(b"\r\n").await?;
            tokio::task::yield_now().await;
        }
        stream.write_all(b"0\r\n\r\n").await?;
    } else {
        stream.write_all(&body).await?;
    }
    stream.shutdown().await
}
