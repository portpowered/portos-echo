//! T3: exact short reply bytes and a withheld satellite acknowledgment.
use super::*;
use echo_contracts::wyoming::{Event, Header, encode};
use tokio::net::{TcpListener, TcpStream};

pub(super) async fn send(stream: &mut TcpStream, kind: &str, data: Value) {
    stream
        .write_all(&encode(&Event {
            header: Header {
                r#type: kind.into(),
                data: Some(data),
                data_length: 0,
                payload_length: 0,
            },
            data: None,
            payload: vec![],
        }))
        .await
        .unwrap();
}

pub(super) async fn read(stream: &mut TcpStream) -> (Value, Vec<u8>) {
    let mut header = Vec::new();
    loop {
        let byte = stream.read_u8().await.unwrap();
        header.push(byte);
        assert!(header.len() <= 8192);
        if byte == b'\n' {
            break;
        }
    }
    let header: Value = serde_json::from_slice(&header).unwrap();
    let mut payload = vec![0; usize::try_from(header["payload_length"].as_u64().unwrap_or(0)).unwrap()];
    stream.read_exact(&mut payload).await.unwrap();
    (header, payload)
}

pub(super) async fn read_reply(stream: &mut TcpStream) -> (Value, Vec<u8>) {
    loop {
        let event = read(stream).await;
        if event.0["type"] != "describe" {
            return event;
        }
        // An idle probe can already be on the wire when the wake arrives.
        // Answer that control request without hiding any unexpected audio.
        assert_eq!(event.1, Vec::<u8>::new());
        send(stream, "info", json!({"satellite":{"name":"synthetic"}})).await;
    }
}

async fn short_reply(acknowledge: bool, frames: u16) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let live = bridge_peer::Live::bind_frames(frames).await;
    let (mut stream, mut bridge) = satellite(&live.url, &path).await;
    assert_eq!(read_reply(&mut stream).await.0["type"], "audio-start");
    receive_short(&mut stream, &live, &path, frames).await;
    complete(&mut stream, &mut bridge, &path, acknowledge).await;
}

pub(super) async fn satellite(url: &str, path: &Path) -> (TcpStream, Process) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let mut bridge = Process::spawn(&mut bridge(&port, url, path));
    let (mut stream, _) = listener.accept().await.unwrap();
    register(&mut stream, &mut bridge).await;
    (stream, bridge)
}

async fn register(stream: &mut TcpStream, bridge: &mut Process) {
    assert_eq!(read(stream).await.0["type"], "run-satellite");
    assert_eq!(read(stream).await.0["type"], "describe");
    send(stream, "info", json!({"satellite":{"name":"synthetic"}})).await;
    assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
    let row: Value = serde_json::from_str(
        include_str!("../../../../tests/fixtures/wyoming/wake-word.jsonl")
            .lines()
            .nth(1)
            .unwrap(),
    )
    .unwrap();
    send(stream, "run-pipeline", row["header"]["data"].clone()).await;
}

pub(super) async fn receive_short(stream: &mut TcpStream, live: &bridge_peer::Live, path: &Path, frames: u16) {
    let previously_played = trace(path).iter().filter(|e| e["event"] == "reply.played").count();
    let mut bytes = Vec::new();
    // Observe admission of the first two frames before closing the source.
    // This barrier tests completion after short audio without RTP/control races.
    for _ in 0..2 {
        let (header, payload) = read_reply(stream).await;
        assert_eq!(header["type"], "audio-chunk");
        bytes.extend(payload);
    }
    live.events
        .send(json!({"type":"response.audio.done"}).to_string())
        .await
        .unwrap();
    let mut chunks = 2;
    loop {
        let (header, payload) = read_reply(stream).await;
        if header["type"] == "audio-stop" {
            break;
        }
        assert_eq!(header["type"], "audio-chunk");
        chunks += 1;
        bytes.extend(payload);
    }
    assert_eq!(chunks, frames);
    assert_eq!(bytes, vec![0; usize::from(frames) * 960]);
    assert_eq!(
        trace(path).iter().filter(|e| e["event"] == "reply.played").count(),
        previously_played
    );
}

async fn complete(stream: &mut TcpStream, bridge: &mut Process, path: &Path, acknowledge: bool) {
    if acknowledge {
        send(stream, "played", json!({})).await;
        assert_eq!(
            serde_json::from_str::<Value>(&bridge.line().await).unwrap(),
            json!({"event":"bridge.run_completed","run_id":"run-1"})
        );
        assert_eq!(trace(path).iter().filter(|e| e["event"] == "reply.played").count(), 1);
        bridge.finish(0).await;
    } else {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(12), bridge.child.wait())
                .await
                .unwrap()
                .unwrap()
                .code(),
            Some(1)
        );
        let entries = trace(path);
        assert_eq!(entries.last().unwrap()["reason"], "deadline");
        assert_eq!(entries.iter().filter(|e| e["event"] == "reply.played").count(), 0);
    }
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_spine_short_reply_requires_real_played() {
    tokio::time::timeout(Duration::from_secs(20), short_reply(true, 3))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_spine_missing_played_is_deadline_not_success() {
    tokio::time::timeout(Duration::from_secs(20), short_reply(false, 3))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_spine_burst_completion_drains_all_decode_batches_before_stop() {
    tokio::time::timeout(Duration::from_secs(20), short_reply(true, 32))
        .await
        .unwrap();
}
