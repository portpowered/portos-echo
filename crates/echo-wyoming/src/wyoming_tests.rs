use crate::wyoming_test_support as wyoming_peer;

use crate::{ClientOptions, Error, WyomingClient, wyoming_wire};
use tokio::io::{AsyncWriteExt, duplex};

#[path = "wyoming_session/wyoming_control_support.rs"]
mod wyoming_control;

#[tokio::test]
async fn cut_discards_unsent_audio_and_keeps_duplex_run_until_played() {
    let (io, peer) = duplex(4096);
    let opts = ClientOptions {
        mic_capacity: 2,
        reply_capacity: 2,
        ..ClientOptions::default()
    };
    wyoming_control::cut_duplex(WyomingClient::from_io(io, opts), peer).await;
}

#[tokio::test]
async fn pause_is_idempotent_and_explicit_registration_resumes_capture() {
    let (io, peer) = duplex(4096);
    wyoming_control::pause_resume(WyomingClient::from_io(io, ClientOptions::default()), peer).await;
}

#[tokio::test]
async fn cancelled_play_invalidates_ownership_before_next_reply() {
    let (io, peer) = duplex(4096);
    wyoming_control::cancel_play(WyomingClient::from_io(io, ClientOptions::default()), peer).await;
}

#[tokio::test]
async fn blocked_reply_writer_retains_only_newest_two_frames() {
    use serde_json::json;
    use tokio::sync::{mpsc, oneshot};
    let (io, mut peer) = duplex(1);
    let opts = ClientOptions {
        reply_capacity: 2,
        ..ClientOptions::default()
    };
    let client = WyomingClient::from_io(io, opts);
    let (tx, rx) = mpsc::channel(1);
    let (ready, barrier) = oneshot::channel();
    let format = serde_json::from_value(json!({"rate":24000,"width":2,"channels":1})).unwrap();
    let producer = async {
        for frame in 1..=5 {
            tx.send(vec![frame]).await.unwrap();
        }
        // Acquiring the slot proves frame 5 was consumed by the pump. It pushes
        // synchronously before polling recv again. The writer is held at start.
        drop(tx.reserve().await.unwrap());
        drop(tx);
        ready.send(()).unwrap();
    };
    let server = async {
        barrier.await.unwrap();
        assert_eq!(wyoming_peer::receive(&mut peer).await.header.r#type, "audio-start");
        for frame in [4, 5] {
            assert_eq!(wyoming_peer::receive(&mut peer).await.payload, [frame]);
        }
        assert_eq!(wyoming_peer::receive(&mut peer).await.header.r#type, "audio-stop");
        wyoming_peer::send(&mut peer, "played", json!(null), vec![]).await;
    };
    let (result, (), ()) = tokio::join!(client.play(format, rx), producer, server);
    assert_eq!(result, Ok(crate::PlayOutcome::Played));
}

#[tokio::test]
async fn playback_is_exclusive_and_stray_played_does_not_complete_it() {
    use echo_contracts::wyoming::{AudioFormat, WyomingEvent};
    use serde_json::json;
    use tokio::sync::mpsc;
    let (io, mut peer) = duplex(4096);
    let mut client = WyomingClient::from_io(io, ClientOptions::default());
    let mut events = client.events().unwrap();
    let format: AudioFormat = serde_json::from_value(json!({"rate":24000,"width":2,"channels":1})).unwrap();
    let (source, frames) = mpsc::channel(1);
    let caller = async {
        let mut play = Box::pin(client.play(format.clone(), frames));
        loop {
            let mut next = Box::pin(events.recv());
            let event = std::future::poll_fn(|cx| {
                assert!(play.as_mut().poll(cx).is_pending());
                next.as_mut().poll(cx)
            })
            .await
            .unwrap()
            .unwrap();
            if matches!(event.event, WyomingEvent::Ping(_)) {
                break;
            }
        }
        let (tx, rx) = mpsc::channel(1);
        drop(tx);
        assert_eq!(client.play(format, rx).await, Err(Error::Busy));
        drop(source);
        assert_eq!(play.await.unwrap(), crate::PlayOutcome::Played);
        client.close().await.unwrap();
    };
    let server = async {
        wyoming_peer::send(&mut peer, "played", json!(null), vec![]).await;
        assert_eq!(wyoming_peer::receive(&mut peer).await.header.r#type, "audio-start");
        wyoming_peer::send(&mut peer, "ping", json!(null), vec![]).await;
        assert_eq!(wyoming_peer::receive(&mut peer).await.header.r#type, "audio-stop");
        wyoming_peer::send(&mut peer, "played", json!(null), vec![]).await;
        assert_eq!(tokio::io::AsyncReadExt::read(&mut peer, &mut [0]).await.unwrap(), 0);
    };
    tokio::join!(caller, server);
}

#[tokio::test]
async fn queue_retains_newest_counts_drops_and_drains_before_error() {
    use crate::{MicFrame, RunStats, wyoming_queue::MicQueue};
    use echo_contracts::wyoming::AudioChunkData;
    let queue = MicQueue::new(2);
    let frame = |value| MicFrame {
        data: AudioChunkData {
            rate: 16000,
            width: 2,
            channels: 1,
            timestamp: None,
            extra: std::collections::BTreeMap::new(),
        },
        payload: vec![value],
    };
    for value in 1..=5 {
        queue.push(frame(value));
    }
    queue.close(Err(Error::Disconnected));
    queue.push(frame(6));
    assert_eq!(
        queue.stats(),
        RunStats {
            received: 5,
            dropped: 3
        }
    );
    assert_eq!(queue.recv().await.unwrap(), Some(frame(4)));
    assert_eq!(queue.recv().await.unwrap(), Some(frame(5)));
    assert_eq!(queue.recv().await, Err(Error::Disconnected));
}

#[tokio::test]
async fn receiver_is_single_owner_and_close_ends_event_stream() {
    let (io, _peer) = duplex(1024);
    let mut client = WyomingClient::from_io(io, ClientOptions::default());
    let mut events = client.events().unwrap();
    assert!(matches!(client.events(), Err(Error::Busy)));
    client.close().await.unwrap();
    assert!(events.recv().await.unwrap().is_none());
    assert_eq!(client.run_satellite().await, Err(Error::Closed));
}

#[tokio::test]
async fn invalid_options_return_before_any_connection_attempt() {
    let opts = ClientOptions {
        mic_capacity: 0,
        ..ClientOptions::default()
    };
    assert!(matches!(
        WyomingClient::connect("127.0.0.1:1".parse().unwrap(), opts).await,
        Err(Error::InvalidOptions)
    ));
}

#[tokio::test]
async fn invalid_wire_limits_and_closed_writer_have_fixed_errors() {
    use echo_contracts::wyoming::WyomingEvent;
    use serde_json::Value;
    for bytes in [b"{not-json}\n".as_slice(), b"123456789", b"{"] {
        let (mut writer, mut reader) = duplex(1024);
        writer.write_all(bytes).await.unwrap();
        drop(writer);
        assert_eq!(wyoming_wire::read(&mut reader, 8).await.unwrap_err(), Error::Protocol);
    }
    let (mut writer, reader) = duplex(1024);
    let event = WyomingEvent::Ping(Value::Null);
    assert_eq!(
        wyoming_wire::write(&mut writer, &event, vec![0; 9], 8).await,
        Err(Error::Capacity)
    );
    assert_eq!(
        wyoming_wire::write(&mut writer, &event, vec![], 8).await,
        Err(Error::Capacity)
    );
    drop(reader);
    assert_eq!(
        wyoming_wire::write(&mut writer, &event, vec![], 1024).await,
        Err(Error::Disconnected)
    );
}

#[tokio::test(start_paused = true)]
async fn unanswered_describe_times_out_and_invalidates_connection() {
    let (io, mut peer) = duplex(1024);
    let client = WyomingClient::from_io(io, ClientOptions::default());
    let caller = async {
        assert_eq!(client.describe().await, Err(Error::Timeout));
    };
    let server = async {
        assert_eq!(wyoming_peer::receive(&mut peer).await.header.r#type, "describe");
    };
    tokio::join!(caller, server);
    assert_eq!(client.run_satellite().await, Err(Error::Closed));
}

#[tokio::test]
async fn duplicate_registration_and_describe_ownership_are_preserved() {
    use echo_contracts::wyoming::WyomingEvent;
    use serde_json::json;
    let (io, mut peer) = duplex(4096);
    let mut client = WyomingClient::from_io(io, ClientOptions::default());
    let mut events = client.events().unwrap();
    let caller = async {
        client.run_satellite().await.unwrap();
        client.run_satellite().await.unwrap();
        let mut first = Box::pin(client.describe());
        let mut next = Box::pin(events.recv());
        let event = std::future::poll_fn(|cx| {
            assert!(first.as_mut().poll(cx).is_pending());
            next.as_mut().poll(cx)
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(event.event, WyomingEvent::Ping(json!("barrier")));
        assert_eq!(client.describe().await, Err(Error::Busy));
        drop(next);
        drop(first);
        client.close().await.unwrap();
    };
    let server = async {
        assert_eq!(wyoming_peer::receive(&mut peer).await.header.r#type, "run-satellite");
        assert_eq!(wyoming_peer::receive(&mut peer).await.header.r#type, "describe");
        wyoming_peer::send(&mut peer, "ping", json!("barrier"), vec![]).await;
        assert_eq!(tokio::io::AsyncReadExt::read(&mut peer, &mut [0]).await.unwrap(), 0);
    };
    tokio::join!(caller, server);
}

#[tokio::test]
async fn event_overflow_is_reported_after_queued_events() {
    let (io, mut peer) = duplex(4096);
    let opts = ClientOptions {
        event_capacity: 1,
        ..ClientOptions::default()
    };
    let mut client = WyomingClient::from_io(io, opts);
    let mut events = client.events().unwrap();
    for _ in 0..2 {
        wyoming_peer::send(&mut peer, "ping", serde_json::Value::Null, vec![]).await;
    }
    // A closed peer is a protocol barrier: the session has terminated.
    assert_eq!(tokio::io::AsyncReadExt::read(&mut peer, &mut [0]).await.unwrap(), 0);
    assert!(matches!(
        events.recv().await.unwrap().unwrap().event,
        echo_contracts::wyoming::WyomingEvent::Ping(_)
    ));
    assert!(matches!(events.recv().await, Err(Error::Capacity)));
}

#[tokio::test]
async fn pipeline_end_and_unscoped_microphone_do_not_mix_runs() {
    use crate::wyoming_route::Router;
    use serde_json::json;
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let mut router = Router {
        current: None,
        events: tx,
        capacity: 2,
    };
    let chunk = || wyoming_peer::frame("audio-chunk", json!({"rate":16000,"width":2,"channels":1}), vec![1]);
    router.route(chunk()).unwrap();
    assert!(rx.recv().await.unwrap().run.is_none());
    router
        .route(wyoming_peer::frame(
            "run-pipeline",
            json!({"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"alexa"}),
            vec![],
        ))
        .unwrap();
    let mut run = rx.recv().await.unwrap().run.unwrap();
    router
        .route(wyoming_peer::frame("audio-stop", json!(null), vec![]))
        .unwrap();
    assert_eq!(run.recv().await.unwrap(), None);
    assert_eq!(run.stats().received, 0);
}

#[tokio::test]
async fn typed_run_replacement_and_fragmented_fixture() {
    let (client, peer) = duplex(4096);
    wyoming_peer::microphone(WyomingClient::from_io(client, ClientOptions::default()), peer).await;
}

#[tokio::test]
async fn empty_minimum_and_ten_chunk_replies_require_played() {
    let (client, peer) = duplex(4096);
    wyoming_peer::playback(WyomingClient::from_io(client, ClientOptions::default()), peer).await;
}

#[tokio::test]
async fn malformed_known_event_fails_before_delivery() {
    let (client, peer) = duplex(4096);
    wyoming_peer::malformed(WyomingClient::from_io(client, ClientOptions::default()), peer).await;
}

#[tokio::test]
async fn oversized_and_truncated_frames_are_protocol_errors() {
    for bytes in [
        b"{\"type\":\"audio-chunk\",\"payload_length\":1000}\n".as_slice(),
        b"{\"type\":\"ping\",\"payload_length\":2}\nx",
    ] {
        let (mut writer, mut reader) = duplex(4096);
        writer.write_all(bytes).await.unwrap();
        drop(writer);
        assert_eq!(wyoming_wire::read(&mut reader, 128).await.unwrap_err(), Error::Protocol);
    }
}

#[test]
fn options_validate_every_zero_and_backoff_order() {
    let valid = ClientOptions::default();
    assert_eq!(valid.validate(), Ok(()));
    let mut cases = vec![];
    for field in 0..9 {
        let mut opts = valid.clone();
        match field {
            0 => opts.mic_capacity = 0,
            1 => opts.reply_capacity = 0,
            2 => opts.event_capacity = 0,
            3 => opts.command_capacity = 0,
            4 => opts.max_frame_bytes = 0,
            5 => opts.connect_timeout = std::time::Duration::ZERO,
            6 => opts.operation_timeout = std::time::Duration::ZERO,
            7 => opts.reconnect_initial = std::time::Duration::ZERO,
            _ => opts.reconnect_cap = std::time::Duration::ZERO,
        }
        cases.push(opts);
    }
    let mut reversed = valid;
    reversed.reconnect_initial = reversed.reconnect_cap * 2;
    cases.push(reversed);
    for opts in cases {
        assert_eq!(opts.validate(), Err(Error::InvalidOptions));
    }
}
