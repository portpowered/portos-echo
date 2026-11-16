//! T3: exact short reply bytes and a withheld satellite acknowledgment.
use super::*;
use echo_contracts::wyoming::{Event, Header, encode};
use tokio::net::{TcpListener, TcpStream};

pub(super) async fn send(stream: &mut (impl tokio::io::AsyncWrite + Unpin), kind: &str, data: Value) {
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

pub(super) async fn read(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> (Value, Vec<u8>) {
    let (header, bytes) = frame(stream).await.unwrap().unwrap();
    let length = usize::try_from(header["payload_length"].as_u64().unwrap_or(0)).unwrap();
    (header, bytes[bytes.len() - length..].to_vec())
}

async fn frame(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> std::io::Result<Option<(Value, Vec<u8>)>> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        if stream.read(&mut byte).await? == 0 {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof))
            };
        }
        bytes.push(byte[0]);
        if bytes.len() > 8192 {
            return Err(std::io::Error::other("satellite header too large"));
        }
        if byte[0] == b'\n' {
            break;
        }
    }
    let header: Value = serde_json::from_slice(&bytes)?;
    let body = ["data_length", "payload_length"]
        .iter()
        .map(|key| usize::try_from(header[key].as_u64().unwrap_or(0)).unwrap())
        .sum::<usize>();
    let start = bytes.len();
    bytes.resize(start + body, 0);
    stream.read_exact(&mut bytes[start..]).await?;
    Ok(Some((header, bytes)))
}

pub(super) async fn read_reply(
    stream: &mut (impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin),
) -> (Value, Vec<u8>) {
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

pub(super) async fn satellite(url: &str, path: &Path) -> (Satellite, Process) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let mut bridge = Process::spawn(&mut bridge(&port, url, path));
    let (mut stream, _) = listener.accept().await.unwrap();
    register(&mut stream, &mut bridge).await;
    (Satellite::new(stream), bridge)
}

pub(super) async fn register(
    stream: &mut (impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin),
    bridge: &mut Process,
) {
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

pub(super) async fn receive_short(
    stream: &mut (impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin),
    live: &bridge_peer::Live,
    path: &Path,
    frames: u16,
) {
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

async fn complete(
    stream: &mut (impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin),
    bridge: &mut Process,
    path: &Path,
    acknowledge: bool,
) {
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

// The observation queue is unbounded only for these finite synthetic workloads:
// a caller withholding audio observations must never block control replies.
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::tcp::OwnedWriteHalf,
    sync::{Mutex, mpsc, oneshot, watch},
    task::JoinHandle,
};
type WriteOperation = Pin<Box<dyn Future<Output = std::io::Result<usize>> + Send>>;
type Observation = std::io::Result<Vec<u8>>;

pub(super) struct Satellite {
    writer: Arc<Mutex<OwnedWriteHalf>>,
    worker: Option<JoinHandle<()>>,
    events: mpsc::UnboundedReceiver<Observation>,
    controls: mpsc::UnboundedSender<(bool, oneshot::Sender<()>)>,
    probes: watch::Receiver<u64>,
    buffered: std::io::Cursor<Vec<u8>>,
    writing: Option<WriteOperation>,
}

impl Satellite {
    pub(super) fn new(socket: TcpStream) -> Self {
        let (reader, writer) = socket.into_split();
        let writer = Arc::new(Mutex::new(writer));
        let (events, receiver) = mpsc::unbounded_channel();
        let (controls, commands) = mpsc::unbounded_channel();
        let (probes, observed) = watch::channel(0);
        let worker_writer = writer.clone();
        let worker = tokio::spawn(async move {
            let result = serve(reader, worker_writer, &events, commands, probes).await;
            if let Err(error) = result {
                let _ = events.send(Err(error));
            }
        });
        Self {
            writer,
            worker: Some(worker),
            events: receiver,
            controls,
            probes: observed,
            buffered: std::io::Cursor::new(Vec::new()),
            writing: None,
        }
    }

    pub(super) async fn responding(&mut self, enabled: bool) {
        let (done, ack) = oneshot::channel();
        self.controls.send((enabled, done)).unwrap();
        ack.await.unwrap();
    }

    pub(super) fn pending_probe(&self) -> bool {
        self.probes.has_changed().unwrap()
    }

    pub(super) async fn probe(&mut self) -> u64 {
        self.probes.changed().await.unwrap();
        *self.probes.borrow_and_update()
    }

    pub(super) async fn close(mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
            let result = worker.await;
            assert!(result.is_ok() || result.unwrap_err().is_cancelled());
        }
        self.writer.lock().await.shutdown().await.unwrap();
    }
}

impl Drop for Satellite {
    fn drop(&mut self) {
        if let Some(worker) = &self.worker {
            worker.abort();
        }
    }
}

async fn serve(
    mut reader: tokio::net::tcp::OwnedReadHalf,
    writer: Arc<Mutex<OwnedWriteHalf>>,
    events: &mpsc::UnboundedSender<Observation>,
    mut controls: mpsc::UnboundedReceiver<(bool, oneshot::Sender<()>)>,
    probes: watch::Sender<u64>,
) -> std::io::Result<()> {
    let mut enabled = true;
    loop {
        // Retain partially read frames across response-mode changes.
        let incoming = frame(&mut reader);
        tokio::pin!(incoming);
        let next = loop {
            tokio::select! {
                result = &mut incoming => break result?,
                Some((value, done)) = controls.recv() => {
                    enabled = value; let _ = done.send(());
                }
            }
        };
        let Some((header, bytes)) = next else {
            return Ok(());
        };
        // Validate full framing/data without rewriting header fields or JSON spacing.
        echo_contracts::wyoming::Decoder::new()
            .feed(&bytes)
            .map_err(std::io::Error::other)?;
        let describe = header["type"] == "describe";
        if describe && enabled {
            let reply = Event {
                header: Header {
                    r#type: "info".into(),
                    data: Some(json!({"satellite":{"name":"synthetic"}})),
                    data_length: 0,
                    payload_length: 0,
                },
                data: None,
                payload: vec![],
            };
            writer.lock().await.write_all(&encode(&reply)).await?;
        } else if events.send(Ok(bytes)).is_err() {
            return Ok(());
        }
        if describe {
            probes.send_modify(|count| *count += 1);
        }
    }
}

impl AsyncRead for Satellite {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.buffered.position() == self.buffered.get_ref().len() as u64 {
            match self.events.poll_recv(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    let Some(worker) = self.worker.as_mut() else {
                        return Poll::Ready(Ok(()));
                    };
                    match Pin::new(worker).poll(cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(result) => {
                            result.unwrap();
                            self.worker = None;
                            return Poll::Ready(Ok(()));
                        }
                    }
                }
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Err(error)),
                Poll::Ready(Some(Ok(bytes))) => self.buffered = std::io::Cursor::new(bytes),
            }
        }
        Pin::new(&mut self.buffered).poll_read(cx, output)
    }
}

impl AsyncWrite for Satellite {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, bytes: &[u8]) -> Poll<std::io::Result<usize>> {
        if self.writing.is_none() {
            let writer = self.writer.clone();
            let bytes = bytes.to_vec();
            self.writing = Some(Box::pin(async move {
                writer.lock().await.write_all(&bytes).await?;
                Ok(bytes.len())
            }));
        }
        let result = self.writing.as_mut().unwrap().as_mut().poll(cx);
        if result.is_ready() {
            self.writing = None;
        }
        result
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if let Some(worker) = &self.worker {
            worker.abort();
        }
        if self.writing.is_none() {
            let writer = self.writer.clone();
            self.writing = Some(Box::pin(async move {
                writer.lock().await.shutdown().await?;
                Ok(0)
            }));
        }
        let result = self.writing.as_mut().unwrap().as_mut().poll(cx);
        if result.is_ready() {
            self.writing = None;
        }
        result.map(|result| result.map(|_| ()))
    }
}

// Keep paused time from auto-advancing past OS socket readiness; scoped to a rig.
pub(super) struct ReadyClock(JoinHandle<()>);
impl ReadyClock {
    pub(super) fn new() -> Self {
        Self(tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        }))
    }
}
impl Drop for ReadyClock {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[tokio::test]
async fn bridge_satellite_probe_preserves_frames_and_drains_before_eof() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut peer = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let mut satellite = Satellite::new(listener.accept().await.unwrap().0);
    let mut expected = Vec::new();
    for kind in [
        "audio-start",
        "audio-chunk",
        "audio-chunk",
        "audio-stop",
        "run-pipeline",
        "error",
    ] {
        let event = Event {
            header: Header {
                r#type: kind.into(),
                data: Some(json!({"inline":1})),
                data_length: 0,
                payload_length: 0,
            },
            data: Some(json!({"external":2})),
            payload: vec![0, 255, 10],
        };
        let bytes = encode(&event);
        peer.write_all(&bytes).await.unwrap();
        expected.extend(bytes);
        send(&mut peer, "describe", json!({})).await;
        assert_eq!(
            read(&mut peer).await,
            (
                json!({"type":"info","data":{"satellite":{"name":"synthetic"}},
            "data_length":0,"payload_length":0}),
                vec![]
            )
        );
    }
    let external = br#"{ "external": 2 }"#;
    let mut bytes = format!(
        "{{\"type\":\"error\",\"extension\":7,\"data_length\":{}}}\n",
        external.len()
    )
    .into_bytes();
    bytes.extend(external);
    peer.write_all(&bytes).await.unwrap();
    expected.extend(bytes);
    send(&mut peer, "describe", json!({})).await;
    assert_eq!(read(&mut peer).await.0["type"], "info");
    peer.shutdown().await.unwrap();
    let mut actual = Vec::new();
    satellite.read_to_end(&mut actual).await.unwrap();
    assert_eq!(actual, expected);
    satellite.close().await;
    assert_eq!(
        peer.read_u8().await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}

#[tokio::test]
async fn bridge_satellite_probe_drop_closes_socket_and_truncation_is_visible() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut peer = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let mut satellite = Satellite::new(listener.accept().await.unwrap().0);
    peer.write_all(b"{\"type\":\"error\",\"payload_length\":2}\nX")
        .await
        .unwrap();
    peer.shutdown().await.unwrap();
    assert_eq!(
        satellite.read_u8().await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
    drop(satellite);
    assert_eq!(
        peer.read_u8().await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}
