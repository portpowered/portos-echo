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

fn chime_frame(index: u32) -> Event {
    let mut event = reply(960);
    event.payload = (index * 480..(index + 1) * 480)
        .flat_map(|sample| {
            let position = sample % 1920;
            let edge = f64::from(position.min(1919 - position).min(240));
            let gain = 983.0 * (1.0 - (std::f64::consts::PI * edge / 240.0).cos()) / 2.0;
            let frequency = if sample < 1920 { 750.0 } else { 1000.0 };
            let sample = (gain * (std::f64::consts::TAU * frequency * f64::from(position) / 24000.0).sin()).round();
            sample.to_string().parse::<i16>().unwrap().to_le_bytes()
        })
        .collect();
    event
}

#[test]
fn earcon_is_distinct_from_reply_and_its_cut_does_not_supply_reply_success() {
    let mut state = Timing {
        run: true,
        ..Timing::default()
    };
    for index in 0..8 {
        state
            .playback(&chime_frame(index), Duration::from_millis(u64::from(index) * 20))
            .unwrap();
    }
    assert_eq!(state.earcon.bytes, 7680);
    assert_eq!(state.earcon.first, Some(Duration::ZERO));
    assert_eq!((state.played, state.nonzero_samples, state.audible_samples), (0, 0, 0));
    assert!(
        state
            .audible_reply(&serde_json::json!({"pcm_bytes":7680,"nonzero_bytes":7000}))
            .is_err()
    );
    state
        .playback(&event("hassmic-continue", 0), Duration::from_millis(360))
        .unwrap();
    state
        .playback(&event("audio-stop", 0), Duration::from_millis(360))
        .unwrap();
    let mut reply = reply(9600);
    reply.payload = 12000_i16.to_le_bytes().repeat(4800);
    state.playback(&reply, Duration::from_millis(400)).unwrap();
    let measured = state
        .audible_reply(&serde_json::json!({"pcm_bytes":9600,"nonzero_bytes":9000}))
        .unwrap();
    assert_eq!(measured["pcm_bytes"], 9600);
    assert_eq!(measured["audible_samples"], 4800);
    assert_eq!(state.first_pcm, Some(Duration::from_millis(400)));
}

#[test]
fn earcon_observer_rejects_corruption_mixing_and_capture_ending_stop() {
    let mut state = Timing {
        run: true,
        ..Timing::default()
    };
    state.playback(&chime_frame(0), Duration::ZERO).unwrap();
    let mut wrong = chime_frame(1);
    wrong.payload[200] ^= 127;
    assert!(state.clone().playback(&wrong, Duration::ZERO).is_err());
    assert!(state.clone().playback(&reply(960), Duration::ZERO).is_err());
    assert!(state.clone().playback(&event("audio-stop", 0), Duration::ZERO).is_err());
    state.playback(&event("hassmic-continue", 0), Duration::ZERO).unwrap();
    state.playback(&event("audio-stop", 0), Duration::ZERO).unwrap();
    state.playback(&reply(960), Duration::ZERO).unwrap();
    assert_eq!((state.earcon.bytes, state.played), (960, 960));
    // An unknown first frame is counted as reply, never silently excluded.
    let mut state = Timing {
        run: true,
        ..Timing::default()
    };
    state.playback(&wrong, Duration::ZERO).unwrap();
    assert_eq!((state.earcon.bytes, state.played), (0, 960));
}

#[test]
fn earcon_flush_settles_chime_without_counting_it_as_reply() {
    let mut state = Timing {
        run: true,
        ..Timing::default()
    };
    state.playback(&chime_frame(0), Duration::ZERO).unwrap();
    let mut corrupt = chime_frame(1);
    corrupt.payload[200] ^= 127;
    assert_eq!(
        state.clone().playback(&corrupt, Duration::ZERO),
        Err("wire: mixed or corrupted earcon PCM".into())
    );
    state
        .playback(&event("hassmic-flush", 0), Duration::from_millis(20))
        .unwrap();
    state
        .playback(&event("hassmic-flush", 0), Duration::from_millis(20))
        .unwrap();
    let mut reply = reply(960);
    reply.payload = 12000_i16.to_le_bytes().repeat(480);
    state.playback(&reply, Duration::from_millis(40)).unwrap();
    assert_eq!(
        (state.earcon.bytes, state.played, state.audible_samples),
        (960, 960, 480)
    );
    assert_eq!(state.first_pcm, Some(Duration::from_millis(40)));
}

#[test]
fn finite_waveform_endpoint_ignores_silence_and_matches_gain_across_frames() {
    let fixture = super::super::world::fixture();
    let mut endpoint = Endpoint::Fixture {
        bytes: signs(&fixture),
        tail: vec![],
    };
    let mut state = Timing::default();
    endpoint
        .observe(&mut state, &event("run-pipeline", 0), Duration::ZERO)
        .unwrap();
    endpoint
        .observe(&mut state, &event("audio-chunk", 25600), Duration::ZERO)
        .unwrap();
    assert_eq!(state.endpoint, None);
    let scaled: Vec<_> = fixture
        .as_chunks::<2>()
        .0
        .iter()
        .flat_map(|bytes| (i16::from_le_bytes(*bytes) * 2).to_le_bytes())
        .collect();
    for (part, time) in [(scaled[..1600].to_vec(), 10), (scaled[1600..].to_vec(), 20)] {
        let mut capture = event("audio-chunk", part.len());
        capture.payload = part;
        endpoint
            .observe(&mut state, &capture, Duration::from_millis(time))
            .unwrap();
        assert_eq!(state.endpoint_seen(), time == 20);
    }
    state.playback(&reply(960), Duration::from_millis(1520)).unwrap();
    assert_eq!(state.sample(), Ok(Duration::from_millis(1500)));
    assert_eq!(state.captured, 28800);
    assert_eq!(
        state.capture_observation(),
        serde_json::json!({"bytes":28800,"chunks":3,
        "endpoint_sample":14400,"earcon_first_frame_present":false})
    );
    assert!(matches!(endpoint, Endpoint::Fixture { tail, .. } if tail.is_empty()));
}

#[test]
fn capture_finds_earcon_samples_across_chunks_without_counting_speaker_data() {
    let mut state = Timing::default();
    state
        .capture(&event("run-pipeline", 0), usize::MAX, Duration::ZERO)
        .unwrap();
    state.playback(&chime_frame(0), Duration::ZERO).unwrap();
    assert_eq!(state.capture_observation()["earcon_first_frame_present"], false);
    let pcm = chime_frame(0).payload;
    for part in pcm.chunks(480) {
        let mut frame = event("audio-chunk", part.len());
        frame.payload = part.to_vec();
        state.capture(&frame, usize::MAX, Duration::ZERO).unwrap();
    }
    assert_eq!(
        state.capture_observation(),
        serde_json::json!({"bytes":960,"chunks":2,
        "endpoint_sample":null,"earcon_first_frame_present":true})
    );
    assert!(state.capture_tail.len() <= 958);
}

#[test]
fn corrupted_or_incomplete_waveform_never_supplies_an_endpoint_and_retention_is_bounded() {
    let fixture = super::super::world::fixture();
    let mut endpoint = Endpoint::Fixture {
        bytes: signs(&fixture),
        tail: vec![],
    };
    let mut state = Timing::default();
    endpoint
        .observe(&mut state, &event("run-pipeline", 0), Duration::ZERO)
        .unwrap();
    let mut corrupted = event("audio-chunk", fixture.len());
    corrupted.payload = fixture;
    corrupted.payload[0] ^= 0x80;
    corrupted.payload[1] ^= 0x80;
    endpoint.observe(&mut state, &corrupted, Duration::ZERO).unwrap();
    for _ in 0..100 {
        endpoint
            .observe(&mut state, &event("audio-chunk", 1600), Duration::ZERO)
            .unwrap();
        assert!(!state.endpoint_seen());
    }
    assert!(matches!(endpoint, Endpoint::Fixture { bytes, tail } if tail.len() < bytes.len()));
    assert_eq!(state.sample(), Err("wire: missing finite utterance endpoint".into()));
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
    let owner = relay.task.abort_handle();
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
    assert!(owner.is_finished(), "relay listener owner survived joined close");
    assert_eq!((timing.captured, timing.played), (4, 6));
    let _sample = timing.sample().unwrap();
    assert_eq!(
        host.read_u8().await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}

#[tokio::test(start_paused = true)]
async fn t3_relay_observes_chime_before_reply_and_preserves_cut_bytes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay = Relay::bind(listener.local_addr().unwrap(), 4).await.unwrap();
    let mut bridge = TcpStream::connect(relay.address).await.unwrap();
    let (mut host, _) = listener.accept().await.unwrap();
    let wake = encode(&event("run-pipeline", 0));
    host.write_all(&wake).await.unwrap();
    let mut buffer = vec![0; wake.len()];
    bridge.read_exact(&mut buffer).await.unwrap();
    assert_eq!(buffer, wake);
    tokio::time::advance(Duration::from_millis(20)).await;
    for value in [chime_frame(0), chime_frame(1), event("hassmic-flush", 0)] {
        let bytes = encode(&value);
        bridge.write_all(&bytes).await.unwrap();
        buffer.resize(bytes.len(), 0);
        host.read_exact(&mut buffer).await.unwrap();
        assert_eq!(buffer, bytes);
    }
    let played = encode(&event("played", 0));
    host.write_all(&played).await.unwrap();
    buffer.resize(played.len(), 0);
    bridge.read_exact(&mut buffer).await.unwrap();
    assert_eq!(buffer, played);
    let mut reply = reply(9600);
    reply.payload = 12000_i16.to_le_bytes().repeat(4800);
    let bytes = encode(&reply);
    bridge.write_all(&bytes).await.unwrap();
    buffer.resize(bytes.len(), 0);
    host.read_exact(&mut buffer).await.unwrap();
    assert_eq!(buffer, bytes);
    let state = relay.close().await.unwrap();
    assert_eq!((state.earcon.bytes, state.played), (1920, 9600));
    assert_eq!(
        state.earcon.first.unwrap().checked_sub(state.wake.unwrap()).unwrap(),
        Duration::from_millis(20)
    );
    assert_eq!(state.audible_samples, 4800);
}

#[tokio::test]
async fn t3_relay_shutdown_during_accept_closes_listener_without_observation() {
    let relay = Relay::bind("127.0.0.1:1".parse().unwrap(), 2).await.unwrap();
    let owner = relay.task.abort_handle();
    let address = relay.address;
    assert_eq!(
        relay.close().await.unwrap().sample(),
        Err("wire: missing finite utterance endpoint".into())
    );
    assert!(owner.is_finished(), "relay listener owner survived joined close");
    for bytes in [0, 1, 65538] {
        assert_eq!(
            Relay::bind(address, bytes).await.err(),
            Some("wire: invalid finite utterance size".into())
        );
    }
}

struct ResetAfter(std::collections::VecDeque<u8>);
impl tokio::io::AsyncRead for ResetAfter {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if let Some(byte) = self.0.pop_front() {
            buffer.put_slice(&[byte]);
            std::task::Poll::Ready(Ok(()))
        } else {
            std::task::Poll::Ready(Err(std::io::ErrorKind::ConnectionReset.into()))
        }
    }
}

#[tokio::test]
async fn reset_is_eof_only_at_a_complete_frame_boundary() {
    assert_eq!(
        frame(&mut ResetAfter(std::collections::VecDeque::new())).await.unwrap(),
        None
    );
    assert_eq!(
        frame(&mut ResetAfter(b"{\"type\":".to_vec().into())).await,
        Err("wire: incomplete header".into())
    );
    let bytes = b"{\"type\":\"played\"}\n".to_vec();
    let mut input = ResetAfter(bytes.clone().into());
    assert_eq!(frame(&mut input).await.unwrap().unwrap().0, bytes);
    assert_eq!(frame(&mut input).await.unwrap(), None);
}

#[test]
fn signed_reply_samples_reject_absent_silent_threshold_only_short_and_wrong_sized_audio() {
    let host = serde_json::json!({"event":"host.ack","pcm_bytes":9600,"nonzero_bytes":9000});
    for (bytes, level, valid) in [
        (0, 0_i16, false),
        (9600, 0, false),
        (9600, 64, false),
        (9600, -64, false),
        (9598, 65, false),
        (9602, -65, false),
        (9600, 65, true),
        (9600, -65, true),
        (9600, i16::MIN, true),
    ] {
        let mut state = Timing {
            run: true,
            ..Timing::default()
        };
        if bytes != 0 {
            let mut event = reply(bytes);
            event.payload = level.to_le_bytes().repeat(bytes / 2);
            state.playback(&event, Duration::ZERO).unwrap();
        }
        assert_eq!(state.audible_reply(&host).is_ok(), valid, "{bytes} bytes at {level}");
        if valid {
            assert_eq!(state.nonzero_samples, 4800);
            assert_eq!(state.audible_samples, 4800);
            assert!(state.audible_reply(&serde_json::json!({})).is_err());
            let mut silent_host = host.clone();
            silent_host["nonzero_bytes"] = 0.into();
            assert!(state.audible_reply(&silent_host).is_err());
        }
    }
}

#[test]
fn signed_measurements_require_both_counts_with_exact_byte_and_sample_bounds() {
    let valid = serde_json::json!({"pcm_bytes":9600,"nonzero_samples":2400,"audible_samples":2400});
    assert_eq!(audible_pcm(&valid), Ok(()));
    for field in ["pcm_bytes", "nonzero_samples", "audible_samples"] {
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(audible_pcm(&missing).is_err(), "missing {field}");
        for value in [
            serde_json::Value::Null,
            serde_json::json!(-1),
            serde_json::json!(2400.5),
        ] {
            let mut malformed = valid.clone();
            malformed[field] = value;
            assert!(audible_pcm(&malformed).is_err(), "invalid {field}");
        }
    }
    for (bytes, nonzero, audible, pass) in [
        (9598, 4800, 4800, false),
        (9602, 4800, 4800, false),
        (9600, 2399, 2399, false),
        (9600, 4801, 4800, false),
        (9600, 2400, 2401, false),
        (9600, 4800, 2399, false),
        (9600, 4800, 4801, false),
        (9600, 4800, 4800, true),
    ] {
        let value = serde_json::json!({"pcm_bytes":bytes,"nonzero_samples":nonzero,"audible_samples":audible});
        assert_eq!(audible_pcm(&value).is_ok(), pass, "{value}");
    }
}
