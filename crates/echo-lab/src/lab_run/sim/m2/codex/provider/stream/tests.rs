//! T1 packet identities and T3 real loopback ICE/DTLS/SRTP across two replies.
use super::Stream;
use echo_live::{AudioFrame, LivePeer};
use std::{io::Write, time::Duration};
use tokio::time::Instant;

#[test]
fn session_stream_rejects_invalid_payload_without_advancing_and_wraps_rtp_fields() {
    let mut stream = Stream {
        sequence: u16::MAX,
        timestamp: u32::MAX - 959,
    };
    assert_eq!(stream.packet(11, false), Err("m2: invalid tone sequence".into()));
    let last = stream.packet(0, false).unwrap();
    assert_eq!((last.sequence, last.timestamp), (u16::MAX, u32::MAX - 959));
    let next = stream.packet(1, false).unwrap();
    assert_eq!((next.sequence, next.timestamp), (0, 0));
    assert_eq!(next.payload, super::super::super::tone::packet(1).unwrap().payload);
}

#[test]
fn session_stream_quiet_edges_repeat_per_reply_without_restarting_transport() {
    let mut stream = Stream::default();
    for reply in 0..2_u16 {
        for index in 0..11 {
            let frame = stream.packet(index, true).unwrap();
            let sequence = reply * 11 + index;
            assert_eq!((frame.sequence, frame.timestamp), (sequence, u32::from(sequence) * 960));
            let payload = if (2..8).contains(&index) {
                super::super::super::tone::packet(index).unwrap().payload
            } else {
                vec![0xf8, 0xff, 0xfe]
            };
            assert_eq!(frame.payload, payload);
        }
    }
}

#[tokio::test]
async fn t3_session_stream_delivers_two_complete_replies_on_one_negotiated_peer() {
    tokio::time::timeout(Duration::from_secs(10), Box::pin(exchange()))
        .await
        .unwrap();
}

async fn exchange() {
    let mut left = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now().into_std())
        .await
        .unwrap();
    let mut right = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now().into_std())
        .await
        .unwrap();
    let offer = left.offer().await.unwrap();
    let answer = right.answer(&offer).await.unwrap();
    left.apply_answer(&answer).await.unwrap();
    while !(left.ready() && right.ready()) {
        progress(&mut left, &mut right).await;
    }
    let mut stream = Stream::default();
    for reply in 0..2 {
        for indices in [0..5, 5..11] {
            burst(&mut left, &mut right, &mut stream, indices, reply).await;
        }
    }
    assert_eq!((left.dropped_audio(), right.dropped_audio()), (0, 0));
    left.close();
    right.close();
    writeln!(
        std::io::stdout(),
        "T3-SESSION-RTP peers=1 replies=2 packets=22 sequence=0..21 timestamps=0..20160 exact_payloads=true drops=0"
    )
    .unwrap();
}

async fn burst(
    left: &mut LivePeer,
    right: &mut LivePeer,
    stream: &mut Stream,
    indices: std::ops::Range<u16>,
    reply: u16,
) {
    let mut expected = Vec::<AudioFrame>::new();
    for index in indices {
        let frame = stream.packet(index, false).unwrap();
        assert_eq!(frame.sequence, reply * 11 + index);
        assert_eq!(frame.timestamp, u32::from(reply * 11 + index) * 960);
        assert_eq!(frame.payload, super::super::super::tone::packet(index).unwrap().payload);
        left.send_audio(frame.clone(), Instant::now().into_std()).await.unwrap();
        expected.push(frame);
    }
    let mut received = Vec::new();
    while received.len() < expected.len() {
        progress(left, right).await;
        while let Some(frame) = right.pop_audio() {
            received.push(frame);
        }
    }
    assert_eq!(received, expected);
}

async fn progress(left: &mut LivePeer, right: &mut LivePeer) {
    let next = left.flush().await.unwrap().min(right.flush().await.unwrap());
    let ready = tokio::select! {
        ready = left.readable() => { ready.unwrap(); 0 },
        ready = right.readable() => { ready.unwrap(); 1 },
        () = tokio::time::sleep_until(next.into()) => 2,
    };
    let now = Instant::now().into_std();
    match ready {
        0 => {
            left.receive(now).await.unwrap();
        }
        1 => {
            right.receive(now).await.unwrap();
        }
        _ => {
            left.timeout(now).await.unwrap();
            right.timeout(now).await.unwrap();
        }
    }
}
