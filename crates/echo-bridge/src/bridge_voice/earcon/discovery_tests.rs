//! T3: production discovery owner, loopback MCP, controlled device/media/stdio.
use super::*;
use crate::bridge_voice::{
    Notice, SessionNotice,
    voice_config::tests::{MemoryPeer, admission_options},
    voice_tools::Tools,
};
use echo_link::{EchoLink, LinkEvent, LinkEvents};
use echo_link::{
    LinkConnector, MicFrame, PcmFormat,
    fake::{Command, FakeConnector, Observer},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::time::Instant;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    sync::oneshot,
};

#[tokio::test]
async fn t3_discovery_consumes_earcon_played_and_preserves_setup_pcm_past_receipt_deadline() {
    Box::pin(exercise(false)).await;
}

#[tokio::test]
async fn t3_discovery_mute_cuts_earcon_before_catalog_is_released() {
    Box::pin(exercise(true)).await;
}

fn frame(value: i16) -> MicFrame {
    MicFrame {
        format: PcmFormat {
            rate: 16000,
            width: 2,
            channels: 1,
        },
        payload: value.to_le_bytes().repeat(320),
    }
}

async fn tools(
    directory: &tempfile::TempDir,
    listener: &TcpListener,
) -> (Tools, crate::bridge_runtime::bridge_screen::Screen) {
    let mut config = echo_config::BridgeConfig::load("../../examples/bridge.example.toml").unwrap();
    config.audit.log_dir = directory.path().join("audit");
    config.mcp.server[0].url = format!("http://{}/mcp", listener.local_addr().unwrap());
    config.mcp.server[0].auth = echo_config::McpAuth::None;
    config.mcp.server[0].bearer_file = None;
    let mut trace = crate::bridge_trace::Trace::open(&directory.path().join("trace"), "api_key").unwrap();
    let screen = crate::bridge_runtime::bridge_screen::Screen::bind(None).await.unwrap();
    let mut tools = Tools::new(&config, Arc::new(|| 0));
    tools.open(&config, &screen, &mut trace, None).await.unwrap();
    (tools, screen)
}

async fn exercise(mute: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let (mut tools, mut screen) = tools(&directory, &listener).await;
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    observer.wake("discovery", vec![frame(123), frame(-456)]);
    let LinkEvent::Wake(run) = events.recv().await.unwrap().unwrap() else {
        unreachable!()
    };
    let (stream, mut remote) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(stream);
    let mut io = echo_codex::codex_io::RpcIo::new(read, write, echo_codex::codex_rpc::Limits::default()).unwrap();
    let (requested, pending) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let acknowledged = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let work = async {
        start().unwrap();
        let options =
            admission_options().conversation(("discovery", "discovery"), Instant::now() + Duration::from_secs(15));
        let result = Box::pin(crate::bridge_voice::voice_tools::conversation(
            (&client, &mut events, run),
            &mut io,
            MemoryPeer,
            (options, &mut tools, Instant::now()),
            std::future::pending(),
            |notice| {
                observe(&notice, &acknowledged, mute);
                Ok(())
            },
        ))
        .await;
        assert_eq!(result, Err(if mute { Error::Control } else { Error::Unavailable }));
        replay().await;
        Ok(())
    };
    let control = control(&observer, pending, release, &acknowledged, mute);
    let (result, (), ()) = tokio::join!(
        scope(&client, true, work),
        control,
        catalog(listener, requested, released)
    );
    assert_eq!(result, Ok(()));
    drop(io);
    let mut bytes = Vec::new();
    remote.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(bytes, Vec::<u8>::new()); // no call starts before discovery succeeds
    receipts(&observer, &acknowledged, mute);
    client.close().await.unwrap();
    tools.close().await;
    screen.close().await.unwrap();
}

fn observe(notice: &SessionNotice, acknowledged: &std::sync::atomic::AtomicUsize, mute: bool) {
    match notice {
        SessionNotice::Run(Notice::EarconPlayed(duration)) => {
            assert!(!mute);
            assert_eq!(*duration, Duration::from_millis(160));
            acknowledged.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        SessionNotice::Run(Notice::Device(LinkEvent::Mute(true))) => {
            assert!(mute);
            acknowledged.store(2, std::sync::atomic::Ordering::SeqCst);
        }
        _ => unreachable!("discovery cannot publish connection or reply"),
    }
}

async fn control(
    observer: &Observer,
    pending: oneshot::Receiver<()>,
    release: oneshot::Sender<()>,
    acknowledged: &std::sync::atomic::AtomicUsize,
    mute: bool,
) {
    pending.await.unwrap();
    tokio::time::pause();
    if mute {
        observer.mute(true);
    }
    advance_until(|| observer.commands().iter().any(|c| matches!(c, Command::Cut(_)))).await;
    let frames = observer
        .commands()
        .iter()
        .filter(|c| matches!(c, Command::Frame(_)))
        .count();
    assert!(if mute { frames < 8 } else { frames == 8 });
    if mute {
        assert_eq!(acknowledged.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
    observer.finish(Ok(()));
    if !mute {
        advance_until(|| acknowledged.load(std::sync::atomic::Ordering::SeqCst) == 1).await;
        assert_eq!(acknowledged.load(std::sync::atomic::Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(3)).await;
    }
    // Release only after acknowledgement survived its unchanged two-second
    // deadline, or after mute has cut the speaker while discovery is held.
    release.send(()).unwrap();
}

async fn catalog(listener: TcpListener, requested: oneshot::Sender<()>, released: oneshot::Receiver<()>) {
    for method in ["initialize", "notifications/initialized", "tools/list"] {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        let mut length = 0;
        loop {
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        stream.read_exact(&mut body).await.unwrap();
        let request: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(request["method"], method);
        if method == "tools/list" {
            requested.send(()).unwrap();
            released.await.unwrap();
            // An explicit discovery refusal ends setup without a fabricated
            // call/reply; retained PCM remains available to the run owner.
            let _ = stream
                .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await;
            return;
        }
        let response = if method == "initialize" {
            json!({"jsonrpc":"2.0","id":request["id"],"result":{"protocolVersion":"2025-03-26", "capabilities":{},"serverInfo":{"name":"synthetic","version":"1"}}}).to_string()
        } else {
            String::new()
        };
        let status = if method == "initialize" {
            "200 OK"
        } else {
            "202 Accepted"
        };
        stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
    }
}

async fn advance_until(ready: impl Fn() -> bool) {
    for _ in 0..150 {
        if ready() {
            return;
        }
        tokio::time::advance(Duration::from_millis(20)).await;
    }
    assert!(ready(), "expected readiness within three simulated seconds");
}

fn receipts(observer: &Observer, acknowledged: &std::sync::atomic::AtomicUsize, mute: bool) {
    assert_eq!(
        acknowledged.load(std::sync::atomic::Ordering::SeqCst),
        if mute { 2 } else { 1 }
    );
    assert_eq!(
        observer
            .commands()
            .iter()
            .filter(|c| matches!(c, Command::Cut(_)))
            .count(),
        1
    );
}

async fn replay() {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    observer.wake_with_mute("empty fallback", vec![], 0);
    let LinkEvent::Wake(mut empty) = events.recv().await.unwrap().unwrap() else {
        unreachable!()
    };
    assert_eq!(microphone(&mut empty).await.unwrap(), Some(frame(123)));
    assert_eq!(microphone(&mut empty).await.unwrap(), Some(frame(-456)));
    assert_eq!(microphone(&mut empty).await.unwrap(), None);
    client.close().await.unwrap();
}
