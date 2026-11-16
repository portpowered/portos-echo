//! Bounded, synthetic Responses peer. It releases commentary only after exact
//! function-call output validation; it never records credentials or bodies.
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{oneshot, watch},
    task::JoinHandle,
};

pub const SUCCESS: &str = "test echo: delegated echo";
pub const FALLBACK: &str = "I couldn't call test echo.";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observation {
    pub requests: usize,
    pub output_verified: bool,
    pub failure: Option<String>,
}

pub struct Model {
    pub origin: String,
    observed: watch::Receiver<Observation>,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl Model {
    #[cfg(test)]
    pub async fn wait_for_result_request(&self) {
        self.observed.clone().wait_for(|o| o.requests == 2).await.unwrap();
    }

    #[cfg(test)]
    pub async fn bind(negative: bool) -> Result<Self, String> {
        Self::bind_after(negative, async { Ok(()) }).await
    }

    #[cfg(test)]
    pub async fn bind_after(
        negative: bool,
        gate: impl std::future::Future<Output = Result<(), String>> + Send + 'static,
    ) -> Result<Self, String> {
        Self::bind_controlled(negative, gate, None).await
    }

    pub async fn bind_controlled(
        negative: bool,
        gate: impl std::future::Future<Output = Result<(), String>> + Send + 'static,
        result_gate: Option<oneshot::Receiver<()>>,
    ) -> Result<Self, String> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| "model: bind failed")?;
        let base = "http://127.0.0.1";
        let port = listener.local_addr().map_err(|_| "model: address unavailable")?.port();
        let origin = format!("{base}:{port}");
        let (observations, observed) = watch::channel(Observation::default());
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            tokio::select! {
                () = async {
                    match gate.await {
                        Ok(()) => serve(listener, negative, &observations, result_gate).await,
                        Err(error) => observations.send_modify(|o| o.failure = Some(error)),
                    }
                } => {},
                _ = stopped => {},
            }
        });
        Ok(Self {
            origin,
            observed,
            stop: Some(stop),
            task,
        })
    }

    pub fn check(&self) -> Result<(), String> {
        let observed = self.observed.borrow();
        if let Some(error) = &observed.failure {
            return Err(error.clone());
        }
        if observed.requests != 2 || !observed.output_verified {
            return Err("model: expected exactly two requests and verified tool output".into());
        }
        Ok(())
    }

    pub async fn close(mut self) -> Result<(), String> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        (&mut self.task).await.map_err(|_| "cleanup: model task failed")?;
        Ok(())
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    listener: TcpListener,
    negative: bool,
    observed: &watch::Sender<Observation>,
    mut result_gate: Option<oneshot::Receiver<()>>,
) {
    let emotion = result_gate.is_some();
    // One request in flight, at most three lifetime accepts. The third records
    // an unexpected retry. Cancellation drops the active socket and listener.
    for index in 0..3 {
        let result = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let (mut socket, _) = listener.accept().await.map_err(|_| "model: accept failed")?;
            let request = read(&mut socket).await?;
            observed.send_modify(|o| o.requests += 1);
            if index == 1 && let Some(gate) = result_gate.take() {
                gate.await.map_err(|_| "model: animation observation abandoned")?;
            }
            let item = response_for(&request, index, negative, emotion)?;
            let body = format!("data: {}\n\ndata: {}\n\n",
                json!({"type":"response.output_item.done","output_index":0,"item":item}),
                json!({"type":"response.completed","response":{"status":"completed"}}));
            let wire = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            socket.write_all(wire.as_bytes()).await.map_err(|_| "model: response failed")?;
            socket.shutdown().await.map_err(|_| "model: response failed")?;
            if index == 1 { observed.send_modify(|o| o.output_verified = true); }
            Ok::<_, &'static str>(())
        }).await.unwrap_or(Err("model: deadline exceeded"));
        if let Err(error) = result {
            observed.send_modify(|o| o.failure = Some(error.into()));
            return;
        }
    }
}

fn call() -> Value {
    json!({"type":"function_call","call_id":"call-1","name":"tool_0","arguments":"{\"text\":\"delegated echo\"}"})
}

fn tools() -> Result<Value, &'static str> {
    let catalog = echo_spoof_mcp::catalog();
    Ok(Value::Array(
        catalog
            .as_array()
            .ok_or("model: malformed spoof catalog")?
            .iter()
            .enumerate()
            .map(|(index, tool)| {
                json!({"type":"function","name":format!("tool_{index}"),"description":tool["description"],
            "parameters":tool["inputSchema"],"strict":false})
            })
            .collect(),
    ))
}

#[cfg(test)]
fn response(request: &Value, index: usize, negative: bool) -> Result<Value, &'static str> {
    response_for(request, index, negative, false)
}

fn response_for(request: &Value, index: usize, negative: bool, emotion: bool) -> Result<Value, &'static str> {
    if index > 1 {
        return Err("model: unexpected extra request");
    }
    if request["model"] != "fixture-model"
        || request["stream"] != true
        || request["store"] != false
        || request["parallel_tool_calls"] != false
        || request["tools"] != tools()?
    {
        return Err("model: request contract mismatch");
    }
    let input = request["input"].as_array().ok_or("model: missing input")?;
    if input.len() != if index == 0 { 2 } else { 4 } || input[0]["role"] != "developer" || input[1]["role"] != "user" {
        return Err("model: input history mismatch");
    }
    let call = if emotion { super::emotion::call() } else { call() };
    if index == 0 {
        return Ok(call);
    }
    if input[2] != call || input[3]["type"] != "function_call_output" || input[3]["call_id"] != "call-1" {
        return Err("model: function call identity mismatch");
    }
    let output: Value = serde_json::from_str(input[3]["output"].as_str().ok_or("model: missing tool output")?)
        .map_err(|_| "model: malformed tool output")?;
    let expected = if negative {
        json!({"content":{"error":"server error: http_status:500"},"is_error":true}) // lint-rules: allow-literal inert tool error text, not a network address
    } else if emotion {
        json!({"content":[{"type":"text","text":super::emotion::state().to_string()}],"is_error":false})
    } else {
        json!({"content":[{"type":"text","text":"delegated echo"}],"is_error":false})
    };
    if output != expected {
        return Err("model: exact tool output mismatch");
    }
    Ok(
        json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":if negative { FALLBACK } else if emotion { super::emotion::REPLY } else { SUCCESS }}]}),
    )
}

async fn read(socket: &mut TcpStream) -> Result<Value, &'static str> {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() == 8192 {
            return Err("model: oversized headers");
        }
        header.push(socket.read_u8().await.map_err(|_| "model: incomplete headers")?);
    }
    let header = std::str::from_utf8(&header).map_err(|_| "model: malformed headers")?;
    let mut lines = header.split("\r\n");
    if lines.next() != Some("POST /v1/responses HTTP/1.1") {
        return Err("model: unexpected route");
    }
    let mut length = None;
    let mut authorized = false;
    for line in lines.filter(|line| !line.is_empty()) {
        let (key, value) = line.split_once(':').ok_or("model: malformed headers")?;
        if key.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err("model: duplicate content length");
            }
            length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| "model: invalid content length")?,
            );
        }
        if key.eq_ignore_ascii_case("transfer-encoding") {
            return Err("model: unexpected transfer encoding");
        }
        if key.eq_ignore_ascii_case("authorization") {
            if authorized || value.trim() != "Bearer synthetic-m4-key" {
                return Err("model: credential mismatch");
            }
            authorized = true;
        }
    }
    if !authorized {
        return Err("model: missing credential");
    }
    let length = length
        .filter(|n| *n <= 65536)
        .ok_or("model: missing or oversized body")?;
    let mut body = vec![0; length];
    socket
        .read_exact(&mut body)
        .await
        .map_err(|_| "model: incomplete body")?;
    serde_json::from_slice(&body).map_err(|_| "model: malformed JSON")
}

#[cfg(test)]
mod tests;
