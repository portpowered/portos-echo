use echo_agent_api::{Task, ToolResult, ToolSpec};
use echo_agent_loop::{CredentialError, ModelCredential};
use echo_config::{AgentConfig, LiveMode};
use echo_contracts::{ids::DelegationId, secret::Secret};
use reqwest::header::{AUTHORIZATION, HeaderMap};
use serde_json::{Value, json};
use std::fmt::Write;
use std::{
    collections::HashMap,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

#[derive(Default)]
pub struct Credentials(pub AtomicUsize);
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

pub fn config() -> AgentConfig {
    AgentConfig {
        model: Some("fixture-model".into()),
        max_tool_calls: 8,
        task_deadline_seconds: 60,
        daily_request_cap: 500,
    }
}
pub fn task() -> Task {
    Task {
        delegation_id: DelegationId::new("delegation-1").unwrap(),
        instructions: "Echo the message".into(),
        context: json!({"message":"hello"}),
        deadline_seconds: 60,
        max_tool_calls: 8,
    }
}
pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "test.echo".into(),
        description: "Echo a message".into(),
        input_schema: json!({"type":"object", "properties":{"message":{"type":"string"}}, "required":["message"], "additionalProperties":false}),
    }
}
pub fn result(index: usize) -> ToolResult {
    ToolResult {
        content: json!({"echo":message(index)}),
        is_error: false,
    }
}
pub fn function(index: usize) -> Value {
    json!({"type":"function_call", "id":format!("fc-{index}"), "call_id":format!("call-{index}"), "name":"tool_0", "arguments":json!({"message":message(index)}).to_string()})
}
pub fn initial() -> Vec<Value> {
    vec![
        json!({"type":"message", "role":"developer", "content":[{"type":"input_text", "text":"Echo the message"}]}),
        json!({"type":"message", "role":"user", "content":[{"type":"input_text", "text":"{\"message\":\"hello\"}"}]}),
    ]
}
pub fn sse(items: &[Value]) -> String {
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

pub struct Request {
    pub start: String,
    pub headers: HashMap<String, String>,
    pub body: Value,
}
pub struct Server {
    pub base: String,
    worker: Option<JoinHandle<Vec<Request>>>,
}
impl Server {
    pub async fn start(responses: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port()); // lint-rules: allow-literal T3 loopback fixture origin, port from the bound 127.0.0.1 listener
        let worker = tokio::spawn(async move {
            let mut requests = Vec::new();
            for body in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                requests.push(read(&mut socket).await);
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
                for fragment in body.as_bytes().chunks(7) {
                    socket.write_all(fragment).await.unwrap();
                }
                socket.shutdown().await.unwrap();
            }
            requests
        });
        Self {
            base,
            worker: Some(worker),
        }
    }
    pub async fn finish(mut self) -> Vec<Request> {
        self.worker.take().unwrap().await.unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(worker) = &self.worker {
            worker.abort();
        }
    }
}

async fn read(socket: &mut TcpStream) -> Request {
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

pub fn message(index: usize) -> String {
    if index == 0 {
        "hello".into()
    } else {
        format!("hello-{index}")
    }
}
