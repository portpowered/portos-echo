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
        client.enable_capture().await.unwrap();
        client.enable_capture().await.unwrap();
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
        assert_eq!(frame.payload, [1, 2, 3, 4]);
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
    assert_eq!(MicrophoneRun::recv(&mut run).await.unwrap().unwrap().payload, [7, 8]);
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
        // A cancellation after the normal stop cannot request continuation.
        assert_eq!(
            EchoLink::cut_with_stats(&client).await.unwrap(),
            CutStats { dropped_frames: 0 }
        );
        send(&mut peer, "played", json!(null), vec![]).await;
    };
    let (result, ()) = tokio::join!(normal, server);
    assert_eq!(result, Ok(PlayOutcome::Played));
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
    for expected in ["hassmic-end", "hassmic-end", "pause-satellite"] {
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
    assert_eq!(next.recv().await.unwrap().unwrap().payload, [1, 2]);
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
