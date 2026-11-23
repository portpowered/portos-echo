use super::*;
use crate::wyoming_test_support::{receive, send};
use serde_json::json;
use tokio::io::{AsyncReadExt, duplex};

fn pcm() -> PcmFormat {
    PcmFormat {
        rate: 24_000,
        width: 2,
        channels: 1,
    }
}

#[test]
fn echo_link_error_categories_and_pcm_boundary_are_exact() {
    for (original, expected) in [
        (crate::Error::InvalidOptions, Error::InvalidOptions),
        (crate::Error::Disconnected, Error::Transport),
        (crate::Error::Protocol, Error::Protocol),
        (crate::Error::Capacity, Error::Capacity),
        (crate::Error::Timeout, Error::Timeout),
        (crate::Error::Busy, Error::Busy),
        (crate::Error::Closed, Error::Closed),
    ] {
        assert_eq!(error(original), expected);
    }
    let format = wire(pcm());
    assert_eq!((format.rate, format.width, format.channels), (24_000, 2, 1));
    assert_eq!(format.extra, std::collections::BTreeMap::default());
}

#[tokio::test(start_paused = true)]
async fn echo_link_capture_probe_wake_reply_pause_close_preserve_wire_order() {
    let (io, peer) = duplex(65536);
    let mut client = WyomingClient::from_io(io, ClientOptions::default());
    let mut events = EchoLink::events(&mut client).unwrap();
    assert_eq!(EchoLink::events(&mut client).err(), Some(Error::Busy));
    let caller = async {
        client.enable_capture(30).await.unwrap();
        client.enable_capture(30).await.unwrap();
        client.probe().await.unwrap();
        assert!(matches!(
            LinkEvents::recv(&mut events).await.unwrap(),
            Some(LinkEvent::Other)
        ));
        let Some(LinkEvent::Wake(mut run)) = LinkEvents::recv(&mut events).await.unwrap() else {
            unreachable!()
        };
        assert_eq!(run.wake_phrase(), "Computer");
        let frame = MicrophoneRun::recv(&mut run).await.unwrap().unwrap();
        assert_eq!(frame.format, PcmFormat { rate: 16_000, ..pcm() });
        assert_eq!(frame.payload.as_ref(), [1, 2, 3, 4]);
        assert_eq!(
            MicrophoneRun::stats(&run),
            RunStats {
                received: 1,
                dropped: 0
            }
        );
        let (tx, rx) = client.reply_channel(NonZeroUsize::new(1).unwrap());
        ReplySink::send(&tx, vec![5, 6]).await.unwrap();
        drop(tx);
        assert_eq!(
            EchoLink::play_with_timeout(&client, pcm(), rx, Duration::from_secs(2)).await,
            Ok(PlayOutcome::Played)
        );
        EchoLink::pause(&client).await.unwrap();
        EchoLink::pause(&client).await.unwrap();
        assert_eq!(MicrophoneRun::recv(&mut run).await.unwrap(), None);
        EchoLink::close(&client).await.unwrap();
    };
    let server = capture_server(peer);
    tokio::join!(caller, server);
}

#[tokio::test(start_paused = true)]
async fn echo_link_empty_reply_waits_for_played_and_timeout_retains_category() {
    let (io, mut peer) = duplex(65536);
    let client = WyomingClient::from_io(io, ClientOptions::default());
    let (tx, rx) = client.reply_channel(NonZeroUsize::new(1).unwrap());
    drop(tx);
    let play = EchoLink::play_with_timeout(&client, pcm(), rx, Duration::from_secs(2));
    tokio::pin!(play);
    let server = async {
        assert_eq!(receive(&mut peer).await.header.r#type, "audio-start");
        assert_eq!(receive(&mut peer).await.header.r#type, "audio-stop");
    };
    tokio::select! { biased; result = &mut play => unreachable!("premature result {result:?}"), () = server => () }
    assert_eq!(play.await, Err(Error::Timeout));
}

#[tokio::test(start_paused = true)]
async fn echo_link_cut_is_idempotent_and_preserves_tail_through_played() {
    let (io, mut peer) = duplex(65536);
    let mut client = WyomingClient::from_io(io, ClientOptions::default());
    let mut events = EchoLink::events(&mut client).unwrap();
    send_wake(&mut peer).await;
    let Some(LinkEvent::Wake(mut run)) = LinkEvents::recv(&mut events).await.unwrap() else {
        unreachable!()
    };
    let (_tx, rx) = client.reply_channel(NonZeroUsize::new(1).unwrap());
    let play = EchoLink::play_with_timeout(&client, pcm(), rx, Duration::from_secs(2));
    let server = async {
        assert_eq!(receive(&mut peer).await.header.r#type, "audio-start");
        for _ in 0..2 {
            assert_eq!(
                EchoLink::cut_with_stats(&client).await.unwrap(),
                CutStats { dropped_frames: 0 }
            );
        }
        assert_eq!(receive(&mut peer).await.header.r#type, "hassmic-flush");
        send(&mut peer, "played", json!(null), vec![]).await;
    };
    let (result, ()) = tokio::join!(play, server);
    assert_eq!(result, Ok(PlayOutcome::Cut));
    assert!(matches!(
        LinkEvents::recv(&mut events).await.unwrap(),
        Some(LinkEvent::Played)
    ));
    send(
        &mut peer,
        "audio-chunk",
        json!({"rate":16000,"width":2,"channels":1}),
        vec![7, 8],
    )
    .await;
    assert_eq!(
        MicrophoneRun::recv(&mut run).await.unwrap().unwrap().payload.as_ref(),
        [7, 8]
    );
    assert!(matches!(
        LinkEvents::recv(&mut events).await.unwrap(),
        Some(LinkEvent::Other)
    ));
    let (tx, rx) = client.reply_channel(NonZeroUsize::new(1).unwrap());
    drop(tx);
    let normal = EchoLink::play_with_timeout(&client, pcm(), rx, Duration::from_secs(2));
    let server = async {
        assert_eq!(receive(&mut peer).await.header.r#type, "audio-start");
        assert_eq!(receive(&mut peer).await.header.r#type, "audio-stop");
        send(&mut peer, "played", json!(null), vec![]).await;
    };
    let (result, ()) = tokio::join!(normal, server);
    assert_eq!(result, Ok(PlayOutcome::Played));
    // Played retires the owner; only then is a subsequent cut a no-op.
    assert_eq!(
        EchoLink::cut_with_stats(&client).await.unwrap(),
        CutStats { dropped_frames: 0 }
    );
    EchoLink::end_run(&client).await.unwrap();
    assert_eq!(receive(&mut peer).await.header.r#type, "hassmic-end");
    assert_eq!(MicrophoneRun::recv(&mut run).await.unwrap(), None);
    EchoLink::close(&client).await.unwrap();
}

async fn capture_server(mut peer: tokio::io::DuplexStream) {
    assert_eq!(receive(&mut peer).await.header.r#type, "run-satellite");
    assert_eq!(receive(&mut peer).await.header.r#type, "describe");
    send(&mut peer, "info", json!({}), vec![]).await;
    send(
        &mut peer,
        "run-pipeline",
        json!({"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"Computer"}),
        vec![],
    )
    .await;
    send(
        &mut peer,
        "audio-chunk",
        json!({"rate":16000,"width":2,"channels":1,"extension":42}),
        vec![1, 2, 3, 4],
    )
    .await;
    reply_server(&mut peer).await;
}

async fn reply_server(peer: &mut tokio::io::DuplexStream) {
    let start = receive(peer).await;
    assert_eq!(start.header.r#type, "audio-start");
    assert_eq!(start.header.data, Some(json!({"rate":24000,"width":2,"channels":1})));
    let chunk = receive(peer).await;
    assert_eq!(chunk.header.r#type, "audio-chunk");
    assert_eq!(chunk.payload, [5, 6]);
    assert_eq!(receive(peer).await.header.r#type, "audio-stop");
    send(peer, "played", json!(null), vec![]).await;
    for expected in ["hassmic-end", "pause-satellite"] {
        assert_eq!(receive(peer).await.header.r#type, expected);
    }
    assert_eq!(peer.read(&mut [0]).await.unwrap(), 0);
}

#[tokio::test(start_paused = true)]
async fn run_failure_literal_frames_then_describe_keep_the_same_stream() {
    let (io, mut peer) = duplex(65536);
    let client = WyomingClient::from_io(io, ClientOptions::default());
    for (cause, expected) in [
        (
            RunFailure::RelinkRequired,
            "{\"data\":{\"code\":\"echo-relink-required\",\"text\":\"Relink required.\"},\"data_length\":0,\"payload_length\":0,\"type\":\"error\"}\n",
        ),
        (
            RunFailure::KillSwitch,
            "{\"data\":{\"code\":\"echo-kill-switch\",\"text\":\"Kill switch enabled.\"},\"data_length\":0,\"payload_length\":0,\"type\":\"error\"}\n",
        ),
        (
            RunFailure::Unavailable,
            "{\"data\":{\"code\":\"echo-unavailable\",\"text\":\"Live unavailable.\"},\"data_length\":0,\"payload_length\":0,\"type\":\"error\"}\n",
        ),
    ] {
        let caller = async {
            client.fail_run(cause).await.unwrap();
            assert_eq!(
                client.describe().await.unwrap(),
                WyomingEvent::Info(json!({"same":true}))
            );
        };
        let server = async {
            assert_eq!(receive(&mut peer).await.header.r#type, "hassmic-end");
            let mut bytes = vec![0; expected.len()];
            peer.read_exact(&mut bytes).await.unwrap();
            assert_eq!(bytes, expected.as_bytes());
            assert_eq!(receive(&mut peer).await.header.r#type, "describe");
            send(&mut peer, "info", json!({"same":true}), vec![]).await;
        };
        tokio::join!(caller, server);
    }
    client.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn run_failure_retires_mic_and_trailing_audio_never_enters_next_run() {
    let (io, mut peer) = duplex(65536);
    let mut client = WyomingClient::from_io(io, ClientOptions::default());
    let mut events = WyomingClient::events(&mut client).unwrap();
    send_wake(&mut peer).await;
    let mut first = events.recv().await.unwrap().unwrap().run.unwrap();
    client.fail_run(RunFailure::Unavailable).await.unwrap();
    assert_eq!(first.recv().await.unwrap(), None);
    assert_eq!(receive(&mut peer).await.header.r#type, "hassmic-end");
    assert_eq!(receive(&mut peer).await.header.r#type, "error");
    send(
        &mut peer,
        "audio-chunk",
        json!({"rate":16000,"width":2,"channels":1}),
        vec![9, 9],
    )
    .await;
    send_wake(&mut peer).await;
    assert!(events.recv().await.unwrap().unwrap().run.is_none());
    let mut next = events.recv().await.unwrap().unwrap().run.unwrap();
    send(
        &mut peer,
        "audio-chunk",
        json!({"rate":16000,"width":2,"channels":1}),
        vec![1, 2],
    )
    .await;
    assert_eq!(next.recv().await.unwrap().unwrap().payload.as_ref(), [1, 2]);
    assert_eq!(first.stats().received, 0);
    client.close().await.unwrap();
}

async fn send_wake(peer: &mut tokio::io::DuplexStream) {
    send(
        peer,
        "run-pipeline",
        json!({"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"Computer"}),
        vec![],
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn run_failure_writer_timeout_capacity_and_closed_are_fixed_errors() {
    use tokio::io::AsyncWriteExt;
    let (io, _stalled) = duplex(1);
    let client = WyomingClient::from_io(io, ClientOptions::default());
    assert_eq!(client.send_error("code", "text").await, Err(crate::Error::Timeout));
    let (io, mut closed) = duplex(4096);
    closed.shutdown().await.unwrap();
    drop(closed);
    let client = WyomingClient::from_io(io, ClientOptions::default());
    assert!(matches!(
        client.send_error("code", "text").await,
        Err(crate::Error::Disconnected | crate::Error::Closed)
    ));
    let (io, _peer) = duplex(4096);
    let client = WyomingClient::from_io(
        io,
        ClientOptions {
            max_frame_bytes: 128,
            ..ClientOptions::default()
        },
    );
    assert_eq!(
        client.send_error("code", &"x".repeat(256)).await,
        Err(crate::Error::Capacity)
    );
}

#[tokio::test(start_paused = true)]
async fn end_run_drains_capture_once_and_preserves_registration_and_next_wake() {
    let (io, mut peer) = duplex(4096);
    let mut client = WyomingClient::from_io(io, ClientOptions::default());
    let mut events = WyomingClient::events(&mut client).unwrap();
    EchoLink::end_run(&client).await.unwrap();
    let caller = async {
        client.run_satellite().await.unwrap();
        for _ in 0..2 {
            let mut run = events.recv().await.unwrap().unwrap().run.unwrap();
            // Observing the coalesced notification proves PCM is queued before end.
            assert_eq!(events.recv().await.unwrap().unwrap().audio_count, 1);
            EchoLink::end_run(&client).await.unwrap();
            EchoLink::end_run(&client).await.unwrap();
            assert_eq!(run.recv().await.unwrap().unwrap().payload.as_ref(), [1, 2]);
            assert_eq!(run.recv().await.unwrap(), None);
            client.run_satellite().await.unwrap();
            client.describe().await.unwrap();
            assert!(matches!(
                events.recv().await.unwrap().unwrap().event,
                WyomingEvent::Info(_)
            ));
        }
        client.close().await.unwrap();
    };
    let server = async {
        assert_eq!(receive(&mut peer).await.header.r#type, "run-satellite");
        for _ in 0..2 {
            send_wake(&mut peer).await;
            send(
                &mut peer,
                "audio-chunk",
                json!({"rate":16000,"width":2,"channels":1}),
                vec![1, 2],
            )
            .await;
            let end = receive(&mut peer).await;
            assert_eq!(end.header.r#type, "hassmic-end");
            assert_eq!(end.header.data, Some(json!(null)));
            assert_eq!(end.payload, Vec::<u8>::new());
            assert_eq!(receive(&mut peer).await.header.r#type, "describe");
            send(&mut peer, "info", json!({}), vec![]).await;
        }
        assert_eq!(peer.read(&mut [0]).await.unwrap(), 0);
    };
    tokio::join!(caller, server);
}

#[tokio::test(start_paused = true)]
async fn end_run_waits_for_cut_played_without_fabricating_acknowledgment() {
    let (io, mut peer) = duplex(4096);
    let mut client = WyomingClient::from_io(io, ClientOptions::default());
    let mut events = WyomingClient::events(&mut client).unwrap();
    send_wake(&mut peer).await;
    let mut run = events.recv().await.unwrap().unwrap().run.unwrap();
    let (tx, rx) = client.reply_channel(NonZeroUsize::MIN);
    drop(tx);
    let play = EchoLink::play_with_timeout(&client, pcm(), rx, Duration::from_secs(2));
    let server = async {
        assert_eq!(receive(&mut peer).await.header.r#type, "audio-start");
        assert_eq!(EchoLink::end_run(&client).await, Err(Error::Busy));
        assert_eq!(receive(&mut peer).await.header.r#type, "audio-stop");
        assert_eq!(EchoLink::end_run(&client).await, Err(Error::Busy));
        client.cut().await.unwrap();
        assert_eq!(receive(&mut peer).await.header.r#type, "hassmic-flush");
        assert_eq!(EchoLink::end_run(&client).await, Err(Error::Busy));
        send(&mut peer, "played", json!(null), vec![]).await;
    };
    let (result, ()) = tokio::join!(play, server);
    assert_eq!(result, Ok(PlayOutcome::Cut));
    EchoLink::end_run(&client).await.unwrap();
    assert_eq!(receive(&mut peer).await.header.r#type, "hassmic-end");
    assert_eq!(run.recv().await.unwrap(), None);
    client.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn end_run_write_timeout_never_closes_capture_successfully() {
    let (io, mut peer) = duplex(1);
    let mut client = WyomingClient::from_io(io, ClientOptions::default());
    let mut events = WyomingClient::events(&mut client).unwrap();
    send_wake(&mut peer).await;
    let mut run = events.recv().await.unwrap().unwrap().run.unwrap();
    // The peer retains its socket but never drains the end frame.
    assert_eq!(EchoLink::end_run(&client).await, Err(Error::Timeout));
    // The request ceiling may cancel the writer before its own ceiling fires.
    // Neither path may turn a partial end write into successful capture EOF.
    assert!(matches!(
        run.recv().await,
        Err(crate::Error::Timeout | crate::Error::Closed)
    ));
}

#[tokio::test(start_paused = true)]
async fn end_run_after_capture_eof_still_ends_device_once() {
    let (io, mut peer) = duplex(4096);
    let mut client = WyomingClient::from_io(io, ClientOptions::default());
    let mut events = WyomingClient::events(&mut client).unwrap();
    send_wake(&mut peer).await;
    let mut run = events.recv().await.unwrap().unwrap().run.unwrap();
    send(&mut peer, "audio-stop", json!(null), vec![]).await;
    assert_eq!(run.recv().await.unwrap(), None);
    EchoLink::end_run(&client).await.unwrap();
    EchoLink::end_run(&client).await.unwrap();
    assert_eq!(receive(&mut peer).await.header.r#type, "hassmic-end");
    client.close().await.unwrap();
    assert_eq!(peer.read(&mut [0]).await.unwrap(), 0);
}

#[tokio::test(start_paused = true)]
async fn played_keeps_capture_through_two_replies_until_explicit_end() {
    let (io, mut peer) = duplex(4096);
    let mut client = WyomingClient::from_io(io, ClientOptions::default());
    let mut events = EchoLink::events(&mut client).unwrap();
    send_wake(&mut peer).await;
    let Some(LinkEvent::Wake(mut run)) = events.recv().await.unwrap() else {
        unreachable!()
    };
    for value in [1, 2] {
        let (tx, rx) = client.reply_channel(NonZeroUsize::MIN);
        drop(tx);
        let play = EchoLink::play_with_timeout(&client, pcm(), rx, Duration::from_secs(2));
        let server = async {
            assert_eq!(receive(&mut peer).await.header.r#type, "audio-start");
            assert_eq!(receive(&mut peer).await.header.r#type, "audio-stop");
            send(&mut peer, "played", json!(null), vec![]).await;
        };
        let (result, ()) = tokio::join!(play, server);
        assert_eq!(result, Ok(PlayOutcome::Played));
        assert!(matches!(events.recv().await.unwrap(), Some(LinkEvent::Played)));
        send(
            &mut peer,
            "audio-chunk",
            json!({"rate":16000,"width":2,"channels":1}),
            vec![value, 1],
        )
        .await;
        assert_eq!(
            MicrophoneRun::recv(&mut run).await.unwrap().unwrap().payload.as_ref(),
            [value, 1]
        );
        assert!(matches!(events.recv().await.unwrap(), Some(LinkEvent::Other)));
    }
    EchoLink::end_run(&client).await.unwrap();
    EchoLink::end_run(&client).await.unwrap();
    assert_eq!(receive(&mut peer).await.header.r#type, "hassmic-end");
    assert_eq!(MicrophoneRun::recv(&mut run).await.unwrap(), None);
    send(
        &mut peer,
        "audio-chunk",
        json!({"rate":16000,"width":2,"channels":1}),
        vec![9, 9],
    )
    .await;
    assert!(matches!(
        events.recv().await.unwrap(),
        Some(LinkEvent::MicrophoneDropped(1))
    ));
    assert_eq!(MicrophoneRun::stats(&run).received, 2);
    EchoLink::close(&client).await.unwrap();
    assert_eq!(peer.read(&mut [0]).await.unwrap(), 0);
}

#[tokio::test(start_paused = true)]
async fn echo_link_thinking_sends_the_display_event_and_nothing_else() {
    let (io, mut peer) = duplex(4096);
    let client = WyomingClient::from_io(io, ClientOptions::default());
    for on in [true, false] {
        EchoLink::thinking(&client, on).await.unwrap();
        let event = receive(&mut peer).await;
        assert_eq!(event.header.r#type, "hassmic-thinking");
        assert_eq!(event.header.data, Some(json!({"thinking":on})));
        assert_eq!(event.payload, Vec::<u8>::new());
    }
}
