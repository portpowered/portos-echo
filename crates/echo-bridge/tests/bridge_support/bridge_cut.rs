//! T3: delivered bridge and codec/WebRTC path, controlled loopback peers.
use super::bridge_short::{read_reply as read, satellite, send};
use super::*;
use std::io::Write;

#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_cut_duplicate_cancel_emits_one_stop_and_waits_for_played() {
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
    send(&mut stream, "played", json!({})).await;
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
    assert!(
        tokio::net::TcpStream::connect(entries[0]["health_addr"].as_str().unwrap())
            .await
            .is_err()
    );
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
    assert_cut_failure(&path, sent, disconnect).await;
}

async fn assert_cut_process_failure(bridge: &mut Process, disconnect: bool) {
    let status = tokio::time::timeout(Duration::from_secs(12), bridge.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code(), Some(1));
    assert_eq!(bridge.lines.next_line().await.unwrap(), None);
    let mut error = String::new();
    bridge
        .child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut error)
        .await
        .unwrap();
    assert_eq!(
        error.trim(),
        if disconnect {
            "echo-bridge: wyoming"
        } else {
            "echo-bridge: deadline"
        }
    );
}

async fn assert_cut_failure(path: &Path, sent: u16, disconnect: bool) {
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
    let address = entries[0]["health_addr"].as_str().unwrap();
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
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
    send(&mut stream, "played", json!({})).await;
    assert_eq!(
        serde_json::from_str::<Value>(&bridge.line().await).unwrap(),
        json!({"event":"bridge.run_completed","run_id":"run-1"})
    );
    assert_cut_trace(&path, offered, sent);
    bridge.finish(0).await;
    assert_closed(&mut stream).await;
}

async fn receive_initial(stream: &mut tokio::net::TcpStream, initial: u16) {
    for _ in 0..initial {
        let (header, payload) = read(stream).await;
        assert_eq!(header["type"], "audio-chunk");
        assert_eq!(payload, vec![0; 960]);
    }
}

async fn drain_to_stop(stream: &mut tokio::net::TcpStream) -> u16 {
    let mut sent = 0;
    loop {
        let (header, payload) = read(stream).await;
        if header["type"] == "audio-stop" {
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

async fn assert_closed(stream: &mut tokio::net::TcpStream) {
    assert_eq!(
        stream.read_u8().await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}

async fn overload_until_observed(
    stream: &mut tokio::net::TcpStream,
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
