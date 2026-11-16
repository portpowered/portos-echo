//! T3 helpers: real Responses HTTP boundary with private synthetic header checks.
use echo_agent_api::{AgentBackend, Task, fakes::FakeToolHost};
use echo_agent_loop::ResponsesBackend;
use echo_config::{AgentConfig, LiveMode};
use echo_contracts::{
    ids::{DelegationId, SessionId},
    task::TaskEvent,
};
use echo_live::{
    LivePeer,
    call::{LiveAuthMode, LiveCallFactory, SessionOptions},
    fake::{FakeLive, FakeResponse, FakeScript},
};
use echo_signin::Providers;
use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

async fn read(socket: &mut TcpStream) -> (String, HashMap<String, String>, serde_json::Value) {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        socket.read_exact(&mut byte).await.unwrap();
        bytes.push(byte[0]);
        assert!(bytes.len() < 16_384);
    }
    let text = String::from_utf8(bytes).unwrap();
    let headers: HashMap<String, String> = text
        .lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.to_lowercase(), value.trim().into()))
        .collect();
    let length: usize = headers["content-length"].parse().unwrap();
    assert!(length < 65_536);
    let mut body = vec![0; length];
    socket.read_exact(&mut body).await.unwrap();
    (
        text.lines().next().unwrap().into(),
        headers,
        serde_json::from_slice(&body).unwrap(),
    )
}

fn model_server(
    listener: TcpListener,
) -> tokio::task::JoinHandle<(String, HashMap<String, String>, serde_json::Value)> {
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let observed = read(&mut socket).await;
        let item =
            serde_json::json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]});
        let body = format!(
            "event: response.output_item.done\r\ndata: {}\r\n\r\nevent: response.completed\r\ndata: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"synthetic\",\"status\":\"completed\"}}}}\r\n\r\n",
            serde_json::json!({"type":"response.output_item.done","output_index":0,"item":item})
        );
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        observed
    })
}

pub async fn model(providers: &Providers, token: &str, account: Option<&str>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = "http://127.0.0.1";
    let base = format!("{origin}:{}", listener.local_addr().unwrap().port());
    let worker = model_server(listener);
    let config = AgentConfig {
        model: Some("synthetic-model".into()),
        max_tool_calls: 8,
        task_deadline_seconds: 60,
        daily_request_cap: 500,
    };
    let host = Arc::new(FakeToolHost::new(Ok(vec![]), vec![]));
    let backend = ResponsesBackend::loopback(
        config,
        providers.mode(),
        host.clone(),
        Arc::new(providers.clone()),
        &base,
    )
    .unwrap();
    let task = Task {
        delegation_id: DelegationId::new("synthetic-delegation").unwrap(),
        instructions: "Reply hello".into(),
        context: serde_json::json!({}),
        deadline_seconds: 60,
        max_tool_calls: 8,
    };
    let id = backend.submit(task).await.unwrap();
    assert_eq!(
        backend.next_event(&id).await,
        Some(TaskEvent::Progress {
            task_id: id.clone(),
            message: "task started".into()
        })
    );
    assert_eq!(
        backend.next_event(&id).await,
        Some(TaskEvent::Completed {
            task_id: id.clone(),
            summary: "hello".into()
        })
    );
    assert_eq!(backend.next_event(&id).await, None);
    assert_eq!(host.calls(), vec![]);
    let (start, headers, body) = worker.await.unwrap();
    check_model(providers.mode(), token, account, &start, &headers, &body);
}

pub async fn live(providers: &Providers, token: &str, account: Option<&str>) {
    let fake = FakeLive::bind(FakeScript {
        events: vec![r#"{"type":"synthetic.ready"}"#.into()],
        response: FakeResponse::Answer,
    })
    .await
    .unwrap();
    let mode = if providers.mode() == LiveMode::Chatgpt {
        LiveAuthMode::Chatgpt
    } else {
        LiveAuthMode::ApiKey
    };
    let now = || tokio::time::Instant::now().into_std();
    let peer = Box::pin(LivePeer::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), now()))
        .await
        .unwrap();
    let factory = LiveCallFactory::new(&fake.base_url(), mode, providers.clone()).unwrap();
    let options = SessionOptions {
        model: if account.is_some() {
            "gpt-live-1-codex"
        } else {
            "gpt-live-1"
        }
        .into(),
        voice: "synthetic".into(),
        instructions: "Reply hello".into(),
        session_id: SessionId::new("synthetic-session").unwrap(),
        thread_id: "synthetic-thread".into(),
    };
    let mut call = factory
        .create(peer, options, now() + Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(
        call.receive_event(now() + Duration::from_secs(10)).await.unwrap(),
        echo_contracts::live::LiveEvent::Other(serde_json::json!({"type":"synthetic.ready"}))
    );
    check_live(&fake, token, account);
    call.close();
    fake.shutdown().await;
}

fn check_live(fake: &FakeLive, token: &str, account: Option<&str>) {
    let calls = fake.observer().calls().unwrap();
    assert_eq!(calls.len(), 1);
    let request = &calls[0].request;
    assert!(request.header_matches("authorization", &format!("Bearer {token}")));
    if let Some(account) = account {
        assert!(request.header_matches("chatgpt-account-id", account));
    } else {
        assert!(!request.has_header("chatgpt-account-id"));
    }
    let path = if account.is_some() {
        "/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas"
    } else {
        "/v1/live/sessions"
    };
    assert_eq!(request.path(), path);
    assert_eq!(
        request.session()["model"],
        if account.is_some() {
            "gpt-live-1-codex"
        } else {
            "gpt-live-1"
        }
    );
}

fn check_model(
    mode: LiveMode,
    token: &str,
    account: Option<&str>,
    start: &str,
    headers: &HashMap<String, String>,
    body: &serde_json::Value,
) {
    let path = if mode == LiveMode::Chatgpt {
        "/backend-api/codex/responses"
    } else {
        "/v1/responses"
    };
    assert_eq!(start, format!("POST {path} HTTP/1.1"));
    assert!(headers["authorization"].eq(&format!("Bearer {token}")));
    assert!(headers.get("chatgpt-account-id").map(String::as_str).eq(&account));
    assert_eq!(body["model"], "synthetic-model");
}
