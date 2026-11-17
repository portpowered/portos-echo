//! T3 component proof: real transports with synthetic providers on 127.0.0.1.
#![cfg(test)]
#[cfg(test)]
mod cases {
    mod codex;
    mod journey;
    mod lifecycle;
    mod limits;
    mod model;
    mod results;
}

use echo_agent_loop::{CredentialError, ModelCredential};
use echo_config::{AgentConfig, LiveMode};
use echo_contracts::secret::Secret;
use reqwest::header::{AUTHORIZATION, HeaderMap};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fmt::Write,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::{io::AsyncReadExt, net::TcpStream};
#[derive(Default)]
pub(crate) struct Credentials(pub(crate) AtomicUsize);
impl ModelCredential for Credentials {
    fn headers(
        &self,
        mode: LiveMode,
    ) -> impl std::future::Future<Output = Result<Secret<HeaderMap>, CredentialError>> + Send {
        let sequence = self.0.fetch_add(1, Ordering::SeqCst);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, format!("Bearer synthetic-{sequence}").parse().unwrap());
        if mode == LiveMode::Chatgpt {
            headers.insert("chatgpt-account-id", "synthetic".parse().unwrap());
        }
        std::future::ready(Ok(Secret::new(headers)))
    }
}

pub(crate) fn config() -> AgentConfig {
    AgentConfig {
        model: Some("fixture-model".into()),
        max_tool_calls: 8,
        task_deadline_seconds: 60,
        daily_request_cap: 500,
    }
}
pub(crate) fn sse(items: &[Value]) -> String {
    let mut stream = String::new();
    for (index, item) in items.iter().enumerate() {
        write!(
            stream,
            "event: response.output_item.done\r\ndata: {}\r\n\r\n",
            json!({"type":"response.output_item.done", "output_index":index, "item":item})
        )
        .unwrap();
    }
    stream.push_str("event: response.completed\r\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"response-1\",\"status\":\"completed\"}}\r\n\r\n");
    stream
}

pub(crate) struct Request {
    pub(crate) start: String,
    pub(crate) headers: HashMap<String, String>,
    pub(crate) body: Value,
}
pub(crate) async fn read(socket: &mut TcpStream) -> Request {
    let mut bytes = Vec::new();
    let end = loop {
        let mut buffer = [0; 1024];
        let count = socket.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let text = std::str::from_utf8(&bytes[..end]).unwrap();
    let mut lines = text.lines();
    let start = lines.next().unwrap().into();
    let headers: HashMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.to_lowercase(), value.trim().into()))
        .collect();
    let length: usize = headers["content-length"].parse().unwrap();
    while bytes.len() < end + length {
        let mut buffer = [0; 1024];
        let count = socket.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&buffer[..count]);
    }
    Request {
        start,
        headers,
        body: serde_json::from_slice(&bytes[end..end + length]).unwrap(),
    }
}
