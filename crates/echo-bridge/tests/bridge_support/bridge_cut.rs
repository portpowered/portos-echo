//! T3: delivered bridge and codec/WebRTC path, controlled loopback peers.
use super::bridge_short::{read_reply as read, satellite, send};
use super::*;
use std::io::Write;
#[path = "bridge_satellite/cut_receipt.rs"]
mod cut_receipt;
#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_cut_duplicate_cancel_emits_one_flush_and_waits_for_played() {
    cut_reply(3, 2).await;
}
#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_cut_nonempty_backlog_conserves_complete_frames() {
    cut_reply(50, 10).await;
}
#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_cut_overload_excludes_earlier_evictions_from_retained_frames() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let live = bridge_peer::Live::bind_frames(32).await;
    let (mut stream, mut bridge) = satellite(&live.url, &path).await;
    assert_eq!(read(&mut stream).await.0["type"], "audio-start");
    receive_initial(&mut stream, 20).await;
    let initial = overload_until_observed(&mut stream, &live, &path).await.unwrap();
    live.events
        .send(json!({"type":"response.done","response":{"status":"cancelled"}}).to_string())
        .await
        .unwrap();
    let sent = initial + drain_to_stop(&mut stream).await;
    assert_eq!(trace(&path).iter().filter(|e| e["event"] == "reply.played").count(), 0);
    complete_capture(&mut stream).await;
    assert_eq!(
        serde_json::from_str::<Value>(&bridge.line().await).unwrap(),
        json!({"event":"bridge.run_completed","run_id":"run-1"})
    );
    let entries = trace(&path);
    let cut = entries.iter().find(|e| e["event"] == "reply.cut").unwrap();
    let evicted = cut["bridge_evicted"].as_u64().unwrap();
    let retained = cut["dropped_frames"].as_u64().unwrap();
    let framed = cut["reply_frames"].as_u64().unwrap();
    assert!(evicted > 0, "fixture must exercise actual bounded eviction");
    assert_eq!(u64::from(sent) + retained + evicted, framed);
    assert_eq!(
        cut["bridge_dropped"].as_u64().unwrap() + cut["transport_dropped"].as_u64().unwrap(),
        retained
    );
    assert!(cut["bridge_dropped"].as_u64().unwrap() <= 128);
    assert_eq!(entries.iter().filter(|e| e["event"] == "reply.cut").count(), 1);
    assert_eq!(entries.iter().filter(|e| e["event"] == "reply.played").count(), 1);
    writeln!(
        std::io::stdout(),
        "overload sent={sent} cut={retained} prior_evicted={evicted} framed={framed}"
    )
    .unwrap();
    bridge.finish(0).await;
    assert_closed(&mut stream).await;
}
#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_cut_missing_played_is_deadline_and_reaps_health() {
    cut_failure(false).await;
}
#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_cut_disconnected_satellite_never_fabricates_played() {
    cut_failure(true).await;
}
#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_cut_control_overflow_closes_receiver_and_reaps_process() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let (ready, request) = tokio::sync::oneshot::channel();
    let (release, gate) = tokio::sync::oneshot::channel();
    let live = bridge_peer::Live::bind_after(0, Some((ready, gate))).await;
    let (mut stream, mut bridge) = satellite(&live.url, &path).await;
    request.await.unwrap(); // The call owner is waiting on signaling, not draining events.
    let mut burst = Vec::new();
    for index in 0..40 {
        burst.extend(echo_contracts::wyoming::encode(&echo_contracts::wyoming::Event {
            header: echo_contracts::wyoming::Header {
                r#type: "ping".into(),
                data: Some(json!({"sequence":index})),
                data_length: 0,
                payload_length: 0,
            },
            data: None,
            payload: vec![],
        }));
    }
    stream.write_all(&burst).await.unwrap();
    // EOF is the session's overflow barrier; no sleep or scheduling assumption.
    assert_eq!(
        stream.read_u8().await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
    release.send(()).unwrap();
    assert_cut_process_failure(&mut bridge, true).await;
    let entries = trace(&path);
    for event in ["reply.cut", "reply.played", "wyoming.run_ended"] {
        assert_eq!(entries.iter().filter(|e| e["event"] == event).count(), 0);
    }
    assert_eq!(entries.last().unwrap()["reason"], "wyoming");
    bridge.assert_collected();
}
async fn cut_failure(disconnect: bool) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let live = bridge_peer::Live::bind_frames(50).await;
    let (mut stream, mut bridge) = satellite(&live.url, &path).await;
    assert_eq!(read(&mut stream).await.0["type"], "audio-start");
    receive_initial(&mut stream, 10).await;
    live.events
        .send(json!({"type":"response.done","response":{"status":"cancelled"}}).to_string())
        .await
        .unwrap();
    let sent = 10 + drain_to_stop(&mut stream).await;
    if disconnect {
        drop(stream);
    }
    assert_cut_process_failure(&mut bridge, disconnect).await;
    assert_cut_failure(&path, sent, disconnect);
    bridge.assert_collected();
}
async fn assert_cut_process_failure(bridge: &mut Process, disconnect: bool) {
    let status = tokio::time::timeout(Duration::from_secs(12), bridge.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code(), Some(1));
    assert_eq!(bridge.lines.next_line().await.unwrap(), None);
    let error = bridge.stderr().await;
    assert_eq!(
        error.trim(),
        if disconnect {
            "echo-bridge: wyoming"
        } else {
            "echo-bridge: deadline"
        }
    );
}
fn assert_cut_failure(path: &Path, sent: u16, disconnect: bool) {
    let entries = trace(path);
    let cuts: Vec<_> = entries.iter().filter(|e| e["event"] == "reply.cut").collect();
    assert_eq!(cuts.len(), 1);
    assert_eq!(u64::from(sent) + cuts[0]["dropped_frames"].as_u64().unwrap(), 49);
    assert_eq!(entries.iter().filter(|e| e["event"] == "reply.played").count(), 0);
    assert_eq!(entries.iter().filter(|e| e["event"] == "wyoming.run_ended").count(), 0);
    assert_eq!(
        entries.last().unwrap()["reason"],
        if disconnect { "wyoming" } else { "deadline" }
    );
}
async fn cut_reply(offered: u16, initial: u16) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let live = bridge_peer::Live::bind_frames(offered).await;
    let (mut stream, mut bridge) = satellite(&live.url, &path).await;
    assert_eq!(read(&mut stream).await.0["type"], "audio-start");
    receive_initial(&mut stream, initial).await;
    for _ in 0..2 {
        live.events
            .send(json!({"type":"response.done","response":{"status":"cancelled"}}).to_string())
            .await
            .unwrap();
    }
    let sent = initial + drain_to_stop(&mut stream).await;
    assert_eq!(trace(&path).iter().filter(|e| e["event"] == "reply.played").count(), 0);
    complete_capture(&mut stream).await;
    assert_eq!(
        serde_json::from_str::<Value>(&bridge.line().await).unwrap(),
        json!({"event":"bridge.run_completed","run_id":"run-1"})
    );
    assert_cut_trace(&path, offered, sent);
    bridge.finish(0).await;
    assert_closed(&mut stream).await;
}
async fn complete_capture(stream: &mut bridge_short::Satellite) {
    send(stream, "played", json!({})).await;
    send(stream, "audio-stop", json!({})).await;
}
async fn receive_initial(stream: &mut bridge_short::Satellite, initial: u16) {
    for _ in 0..initial {
        let (header, payload) = read(stream).await;
        assert_eq!(header["type"], "audio-chunk");
        assert_eq!(payload, vec![0; 960]);
    }
}
pub(super) async fn drain_to_stop(stream: &mut (impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin)) -> u16 {
    let mut sent = 0;
    loop {
        let (header, payload) = read(stream).await;
        if header["type"] == "hassmic-flush" {
            assert_eq!(
                header,
                json!({"type":"hassmic-flush","data":null,"data_length":0,"payload_length":0})
            );
            assert_eq!(payload, Vec::<u8>::new());
            break;
        }
        assert_eq!(header["type"], "audio-chunk");
        assert_eq!(payload, vec![0; 960]);
        sent += 1;
    }
    sent
}
fn assert_cut_trace(path: &Path, offered: u16, sent: u16) {
    let entries = trace(path);
    let cuts: Vec<_> = entries.iter().filter(|e| e["event"] == "reply.cut").collect();
    assert_eq!(cuts.len(), 1);
    let bridge_dropped = cuts[0]["bridge_dropped"].as_u64().unwrap();
    let transport_dropped = cuts[0]["transport_dropped"].as_u64().unwrap();
    let total = cuts[0]["dropped_frames"].as_u64().unwrap();
    assert_eq!(bridge_dropped + transport_dropped, total);
    // The resampler retains a partial frame until normal completion. A cut
    // discards that partial tail but counts only complete accepted frames.
    assert_eq!(u64::from(sent) + total, u64::from(offered - 1));
    let played: Vec<_> = entries.iter().filter(|e| e["event"] == "reply.played").collect();
    assert_eq!(played.len(), 1);
    assert_eq!(played[0]["outcome"], "cut");
    assert_eq!(played[0]["run_id"], "run-1");
}
async fn assert_closed(stream: &mut bridge_short::Satellite) {
    // After a completed run the bridge's idle loop probes the link with
    // `describe` at once; one can reach the wire before stdin EOF stops the
    // bridge. Allow only those empty control probes before EOF, never audio.
    loop {
        let mut byte = [0];
        let first = match stream.read(&mut byte).await {
            Ok(0) => return,
            Ok(1) => byte[0],
            Ok(_) => unreachable!(),
            Err(error) => {
                assert!(bridge_short::terminal(&error, true), "{error:?}");
                return;
            }
        };
        let mut header = vec![first];
        while *header.last().unwrap() != b'\n' {
            header.push(stream.read_u8().await.unwrap());
            assert!(header.len() <= 8192);
        }
        let header: Value = serde_json::from_slice(&header).unwrap();
        assert_eq!(header["type"], "describe", "only an idle probe may precede EOF");
        assert_eq!(header["payload_length"].as_u64().unwrap_or(0), 0);
        let data = usize::try_from(header["data_length"].as_u64().unwrap_or(0)).unwrap();
        stream.read_exact(&mut vec![0; data]).await.unwrap();
    }
}
async fn overload_until_observed(
    stream: &mut bridge_short::Satellite,
    live: &bridge_peer::Live,
    path: &Path,
) -> Option<u16> {
    let mut sent = 20;
    // Generate bounded load until the production queue reports its first
    // eviction. Socket observations pace batches; UDP delivery is not assumed.
    for _ in 0..64 {
        live.events.send("synthetic.more_audio".into()).await.unwrap();
        receive_initial(stream, 1).await;
        sent += 1;
        if trace(path).iter().any(|e| e["event"] == "reply.overflow") {
            return Some(sent);
        }
    }
    None
}
struct Progress<R> {
    reader: R,
    remaining: usize,
    reached: Option<tokio::sync::oneshot::Sender<()>>,
}
impl<R: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for Progress<R> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let result = std::pin::Pin::new(&mut self.reader).poll_read(cx, buf);
        self.remaining = self.remaining.saturating_sub(buf.filled().len() - before);
        if self.remaining == 0
            && let Some(done) = self.reached.take()
        {
            let _ = done.send(());
        }
        result
    }
}
#[tokio::test]
async fn bridge_satellite_shutdown_retains_partial_frame_and_fifo() {
    tokio::time::timeout(Duration::from_secs(5), async {
        for split in [5, 42] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap();
            let bytes = b"{\"type\":\"audio-chunk\",\"payload_length\":3}\nXYZ";
            let mut expected = bytes.repeat(3);
            let (done, reached) = tokio::sync::oneshot::channel();
            let mut stream =
                bridge_short::Satellite::with_reader(listener.accept().await.unwrap().0, |reader| Progress {
                    reader,
                    remaining: expected.len() + split,
                    reached: Some(done),
                });
            peer.write_all(&expected).await.unwrap();
            peer.write_all(&bytes[..split]).await.unwrap();
            reached.await.unwrap(); // Reader has consumed bytes inside the fourth frame.
            stream.responding(false).await;
            stream.responding(true).await; // Same in-flight frame survives mode changes.
            let mut actual = vec![stream.read_u8().await.unwrap()];
            stream.shutdown().await.unwrap();
            assert_eq!(
                peer.read_u8().await.unwrap_err().kind(),
                std::io::ErrorKind::UnexpectedEof
            );
            peer.write_all(&bytes[split..]).await.unwrap();
            peer.shutdown().await.unwrap();
            expected.extend(bytes);
            stream.read_to_end(&mut actual).await.unwrap();
            assert_eq!(actual, expected);
            stream.shutdown().await.unwrap();
            stream.close().await.unwrap();
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn bridge_satellite_probe_preserves_frames_and_drains_before_eof() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let mut satellite = bridge_short::Satellite::new(listener.accept().await.unwrap().0);
    let mut expected = Vec::new();
    for (index, kind) in [
        "audio-start",
        "audio-chunk",
        "audio-chunk",
        "audio-stop",
        "run-pipeline",
        "error",
    ]
    .into_iter()
    .enumerate()
    {
        let event = echo_contracts::wyoming::Event {
            header: echo_contracts::wyoming::Header {
                r#type: kind.into(),
                data: Some(json!({"inline":1})),
                data_length: 0,
                payload_length: 0,
            },
            data: Some(json!({"external":2})),
            payload: vec![u8::try_from(index).unwrap(), 255, 10],
        };
        let bytes = echo_contracts::wyoming::encode(&event);
        peer.write_all(&bytes).await.unwrap();
        expected.extend(bytes);
        send(&mut peer, "describe", json!({})).await;
        assert_eq!(
            bridge_short::read(&mut peer).await,
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
    assert_eq!(bridge_short::read(&mut peer).await.0["type"], "info");
    peer.shutdown().await.unwrap();
    let mut actual = Vec::new();
    satellite.read_to_end(&mut actual).await.unwrap();
    assert_eq!(actual, expected);
    satellite.close().await.unwrap();
    assert_eq!(
        peer.read_u8().await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}
#[tokio::test]
async fn bridge_satellite_probe_drop_closes_socket_and_truncation_is_visible() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let mut satellite = bridge_short::Satellite::new(listener.accept().await.unwrap().0);
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
#[tokio::test]
async fn bridge_satellite_abandonment_reaps_worker_and_socket() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    for abandon in [false, true] {
        let mut peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let stream = bridge_short::Satellite::new(listener.accept().await.unwrap().0);
        let worker = stream.worker_handle();
        if abandon {
            assert_eq!(
                stream.close().await.unwrap_err().kind(),
                std::io::ErrorKind::Interrupted
            );
        } else {
            drop(stream);
        }
        assert_eq!(
            peer.read_u8().await.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        assert!(worker.is_finished());
    }
}
// Keep paused time from auto-advancing past OS socket readiness; scoped to a rig.
pub(super) struct ReadyClock(tokio::task::JoinHandle<()>);
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
async fn bridge_satellite_socket_errors_follow_accepted_frames() {
    use std::io::ErrorKind::{BrokenPipe, InvalidData, UnexpectedEof};
    for (tail, kind) in [
        (b"{".as_slice(), UnexpectedEof),
        (b"{\"type\":\"error\",\"data_length\":2}\nX", UnexpectedEof),
        (b"{\"type\":\"error\",\"payload_length\":2}\nX", UnexpectedEof),
        (b"invalid\n", InvalidData),
        (b"{\"type\":\"describe\"}\n", BrokenPipe),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let mut stream = bridge_short::Satellite::new(listener.accept().await.unwrap().0);
        stream.shutdown().await.unwrap(); // A later probe reply must surface its write failure.
        let accepted = b"{\"type\":\"error\"}\n";
        peer.write_all(accepted).await.unwrap();
        peer.write_all(tail).await.unwrap();
        peer.shutdown().await.unwrap();
        let mut actual = vec![];
        let error = stream.read_to_end(&mut actual).await.unwrap_err();
        assert_eq!(actual, accepted);
        assert_eq!(error.kind(), kind);
        assert!(!bridge_short::terminal(&error, true));
        assert_eq!(
            stream.close().await.unwrap_err().kind(),
            std::io::ErrorKind::Interrupted
        );
    }
}
