use super::*;
use echo_contracts::wyoming::encode;

fn event(name: &str, bytes: usize) -> Event {
    Event {
        header: Header {
            r#type: name.into(),
            data: Some(serde_json::json!({"rate":16000,"width":2,"channels":1})),
            data_length: 0,
            payload_length: bytes,
        },
        data: None,
        payload: vec![0; bytes],
    }
}

fn reply(bytes: usize) -> Event {
    let mut value = event("audio-chunk", bytes);
    value.header.data = Some(serde_json::json!({"rate":24000,"width":2,"channels":1}));
    value
}

#[test]
fn endpoint_requires_current_run_and_complete_finite_capture() {
    let mut state = Timing::default();
    state.capture(&event("audio-chunk", 4), 4, Duration::ZERO).unwrap();
    assert_eq!(state.captured, 0);
    assert_eq!(state.sample(), Err("wire: missing finite utterance endpoint".into()));
    state.capture(&event("run-pipeline", 0), 4, Duration::ZERO).unwrap();
    state
        .capture(&event("audio-chunk", 2), 4, Duration::from_millis(10))
        .unwrap();
    assert_eq!(state.endpoint, None);
    state
        .capture(&event("audio-chunk", 2), 4, Duration::from_millis(20))
        .unwrap();
    assert_eq!(state.sample(), Err("wire: missing reply PCM".into()));
    state
        .capture(&event("audio-chunk", 4), 4, Duration::from_millis(30))
        .unwrap();
    state.playback(&reply(6), Duration::from_millis(1520)).unwrap();
    state.playback(&reply(8), Duration::from_millis(1530)).unwrap();
    assert_eq!((state.captured, state.played), (8, 14));
    assert_eq!(state.sample(), Ok(Duration::from_millis(1500)));
    assert_eq!(
        state.capture(&event("run-pipeline", 0), 4, Duration::ZERO),
        Err("wire: multiple runs in one world".into())
    );
}

#[test]
fn early_missing_invalid_pcm_and_transport_failure_cannot_supply_sample() {
    let mut state = Timing::default();
    assert_eq!(
        state.playback(&reply(2), Duration::ZERO),
        Err("wire: invalid reply PCM".into())
    );
    state.capture(&event("run-pipeline", 0), 2, Duration::ZERO).unwrap();
    for size in [0, 1, 3] {
        assert_eq!(
            state.playback(&reply(size), Duration::ZERO),
            Err("wire: invalid reply PCM".into())
        );
    }
    state.playback(&reply(2), Duration::from_millis(1)).unwrap();
    state
        .capture(&event("audio-chunk", 2), 2, Duration::from_millis(2))
        .unwrap();
    assert_eq!(state.sample(), Err("wire: reply precedes utterance endpoint".into()));
    state.failure = Some("wire: forwarding failed".into());
    assert_eq!(state.sample(), Err("wire: forwarding failed".into()));
}

#[test]
fn endpoint_cannot_be_estimated_inside_a_larger_capture_frame() {
    let mut state = Timing::default();
    state.capture(&event("run-pipeline", 0), 2, Duration::ZERO).unwrap();
    assert_eq!(
        state.capture(&event("audio-chunk", 4), 2, Duration::ZERO),
        Err("wire: utterance endpoint is not a frame boundary".into())
    );
    assert_eq!(state.endpoint, None);
}

#[test]
fn nearest_rank_p95_preserves_the_budget_boundary_and_rejects_missing_samples() {
    assert_eq!(p95(&[]), Err("wire: no latency samples".into()));
    assert_eq!(p95(&[Duration::from_millis(1500)]), Ok(Duration::from_millis(1500)));
    assert_eq!(p95(&[Duration::from_millis(1501)]), Ok(Duration::from_millis(1501)));
    for count in [2, 19, 20, 21, 40, 101] {
        let samples: Vec<_> = (1..=count).rev().map(Duration::from_millis).collect();
        assert_eq!(p95(&samples), Ok(Duration::from_millis((count * 95).div_ceil(100))));
    }
}

#[tokio::test]
async fn bounded_reader_preserves_bytes_and_rejects_partial_oversize_or_malformed_frames() {
    let value = event("audio-chunk", 12);
    let bytes = encode(&value);
    let (actual, decoded) = frame(&mut bytes.as_slice()).await.unwrap().unwrap();
    assert_eq!(actual, bytes);
    assert_eq!(decoded, value);
    assert_eq!(frame(&mut b"".as_slice()).await.unwrap(), None);
    for bytes in [
        b"{".to_vec(),
        b"{}\n".to_vec(),
        vec![b'x'; 4097],
        b"{\"type\":\"audio-chunk\",\"payload_length\":65537}\n".to_vec(),
        b"{\"type\":\"audio-chunk\",\"payload_length\":2}\nx".to_vec(),
        b"{\"type\":\"unknown\",\"data_length\":1}\nx".to_vec(),
    ] {
        assert!(frame(&mut bytes.as_slice()).await.is_err(), "{bytes:?}");
    }
}

// T3 loopback proves both socket directions preserve exact wire bytes, and
// shutdown closes the admitted host connection and the single-use listener.
#[tokio::test]
async fn t3_relay_forwards_both_directions_and_reaps_on_close() {
    let host = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay = Relay::bind(host.local_addr().unwrap(), 4).await.unwrap();
    let address = relay.address;
    let mut bridge = TcpStream::connect(address).await.unwrap();
    let (mut host, _) = host.accept().await.unwrap();
    for value in [event("run-pipeline", 0), event("audio-chunk", 4)] {
        let bytes = encode(&value);
        host.write_all(&bytes).await.unwrap();
        let mut actual = vec![0; bytes.len()];
        bridge.read_exact(&mut actual).await.unwrap();
        assert_eq!(actual, bytes);
    }
    let reply = encode(&reply(6));
    bridge.write_all(&reply).await.unwrap();
    let mut actual = vec![0; reply.len()];
    host.read_exact(&mut actual).await.unwrap();
    assert_eq!(actual, reply);
    let timing = relay.close().await.unwrap();
    assert_eq!((timing.captured, timing.played), (4, 6));
    let _sample = timing.sample().unwrap();
    assert_eq!(
        host.read_u8().await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
    assert!(TcpStream::connect(address).await.is_err());
}

#[tokio::test]
async fn t3_relay_shutdown_during_accept_closes_listener_without_observation() {
    let relay = Relay::bind("127.0.0.1:1".parse().unwrap(), 2).await.unwrap();
    let address = relay.address;
    assert_eq!(
        relay.close().await.unwrap().sample(),
        Err("wire: missing finite utterance endpoint".into())
    );
    assert!(TcpStream::connect(address).await.is_err());
    for bytes in [0, 1, 65538] {
        assert_eq!(
            Relay::bind(address, bytes).await.err(),
            Some("wire: invalid finite utterance size".into())
        );
    }
}
