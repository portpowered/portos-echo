//! T1: public cut boundary over in-memory duplex, with no sockets or clock sleeps.
use crate::{ClientOptions, CutStats, Error, PlayOutcome, WyomingClient, wyoming_test_support as peer};
use serde_json::json;
use std::sync::Arc;
use tokio::{io::duplex, sync::mpsc};

#[tokio::test]
async fn failure_after_cut_preserves_real_played_and_completes_capture() {
    let (io, mut stream) = duplex(4096);
    let mut client = WyomingClient::from_io(io, ClientOptions::default());
    let mut events = client.events().unwrap();
    let client = Arc::new(client);
    peer::send(
        &mut stream,
        "run-pipeline",
        json!({"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"alexa"}),
        vec![],
    )
    .await;
    let mut run = events.recv().await.unwrap().unwrap().run.unwrap();
    peer::send(
        &mut stream,
        "audio-chunk",
        json!({"rate":16000,"width":2,"channels":1}),
        vec![1, 2],
    )
    .await;
    assert_eq!(run.recv().await.unwrap().unwrap().payload.as_ref(), vec![1, 2]);
    let (_tx, rx) = mpsc::channel(1);
    let format = serde_json::from_value(json!({"rate":24000,"width":2,"channels":1})).unwrap();
    let owner = client.clone();
    let play = tokio::spawn(async move { owner.play(format, rx).await });
    assert_eq!(peer::receive(&mut stream).await.header.r#type, "audio-start");
    client.cut_with_stats().await.unwrap();
    client.send_error("echo-killed", "Kill switch enabled.").await.unwrap();
    for name in ["hassmic-flush", "hassmic-end", "error"] {
        let frame = peer::receive(&mut stream).await;
        assert_eq!(frame.header.r#type, name);
        assert_eq!(frame.payload, Vec::<u8>::new());
        if name != "error" {
            assert_eq!(frame.header.data, Some(serde_json::Value::Null));
            assert_eq!(frame.header.payload_length, 0);
        }
    }
    // The failure ends capture, but only the satellite can settle the cut.
    assert_eq!(run.recv().await.unwrap(), None);
    assert_eq!(run.stats().received, 1);
    assert_eq!(run.stats().dropped, 0);
    assert!(
        !play.is_finished(),
        "failure must retain the outstanding Played receipt"
    );
    peer::send(&mut stream, "played", json!({}), vec![]).await;
    assert_eq!(play.await.unwrap(), Ok(PlayOutcome::Cut));
    client.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn missing_played_deadline_retires_failed_run_and_serves_next_run() {
    let (io, mut stream) = duplex(4096);
    let mut client = WyomingClient::from_io(io, ClientOptions::default());
    let mut events = client.events().unwrap();
    let format = serde_json::from_value(json!({"rate":24000,"width":2,"channels":1})).unwrap();
    let (tx, rx) = mpsc::channel(1);
    drop(tx);
    let mut play = Box::pin(client.play(format, rx));
    let stop = async {
        assert_eq!(peer::receive(&mut stream).await.header.r#type, "audio-start");
        assert_eq!(peer::receive(&mut stream).await.header.r#type, "audio-stop");
    };
    tokio::select! { biased; result = &mut play => unreachable!("unexpected {result:?}"), () = stop => () }
    // Model the journey's control deadline, not the client's transport timeout.
    let expired = tokio::time::timeout(std::time::Duration::from_secs(1), &mut play).await;
    assert!(expired.is_err());
    drop(play);
    let caller = async {
        report_failure(&client).await;
        let mut run = events.recv().await.unwrap().unwrap().run.unwrap();
        assert_eq!(run.recv().await.unwrap().unwrap().payload.as_ref(), vec![1, 2]);
        let (tx, rx) = mpsc::channel(1);
        tx.send(vec![3, 4]).await.unwrap();
        drop(tx);
        let format = serde_json::from_value(json!({"rate":24000,"width":2,"channels":1})).unwrap();
        assert_eq!(client.play(format, rx).await, Ok(PlayOutcome::Played));
        client.close().await.unwrap();
    };
    let server = async {
        for _ in 0..2 {
            assert_eq!(peer::receive(&mut stream).await.header.r#type, "hassmic-end");
        }
        let error = peer::receive(&mut stream).await;
        assert_eq!(error.header.r#type, "error");
        assert_eq!(
            error.header.data.unwrap(),
            json!({"code":"echo-unavailable","text":"Live unavailable."})
        );
        peer::send(
            &mut stream,
            "run-pipeline",
            json!({"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"alexa"}),
            vec![],
        )
        .await;
        peer::send(
            &mut stream,
            "audio-chunk",
            json!({"rate":16000,"width":2,"channels":1}),
            vec![1, 2],
        )
        .await;
        assert_eq!(peer::receive(&mut stream).await.header.r#type, "audio-start");
        assert_eq!(peer::receive(&mut stream).await.payload, vec![3, 4]);
        assert_eq!(peer::receive(&mut stream).await.header.r#type, "audio-stop");
        peer::send(&mut stream, "played", json!({}), vec![]).await;
        assert_eq!(peer::receive(&mut stream).await.header.r#type, "hassmic-end");
        // Keep the duplex peer alive until the caller has consumed Played.
        assert_eq!(tokio::io::AsyncReadExt::read(&mut stream, &mut [0]).await.unwrap(), 0);
    };
    tokio::join!(caller, server);
}

#[tokio::test]
async fn exact_cut_before_first_send_drops_every_frame_once() {
    use std::{future::Future, task::Poll};
    let (io, mut stream) = duplex(256);
    let client = WyomingClient::from_io(io, ClientOptions::default());
    let (tx, rx) = mpsc::channel(3);
    for value in 0..3 {
        tx.send(vec![value; 40]).await.unwrap();
    }
    let format = serde_json::from_value(json!({"rate":24000,"width":2,"channels":1})).unwrap();
    let mut play = Box::pin(client.play(format, rx));
    std::future::poll_fn(|cx| {
        assert!(play.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let mut cut = Box::pin(client.cut_with_stats());
    std::future::poll_fn(|cx| {
        assert!(cut.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    // Both commands precede session scheduling. Command priority makes the cut
    // linearize before the first audio write, regardless of pump scheduling.
    assert_eq!(peer::receive(&mut stream).await.header.r#type, "audio-start");
    assert_eq!(peer::receive(&mut stream).await.header.r#type, "hassmic-flush");
    assert_eq!(cut.await.unwrap(), CutStats { dropped_frames: 3 });
    assert!(tx.is_closed());
    peer::send(&mut stream, "played", json!({}), vec![]).await;
    assert_eq!(play.await, Ok(PlayOutcome::Cut));
    assert_eq!(client.cut_with_stats().await.unwrap(), CutStats::default());
    client.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn exact_cut_stalled_stop_reports_timeout_without_statistics_or_played() {
    let (io, mut stream) = duplex(1);
    let client = Arc::new(WyomingClient::from_io(io, ClientOptions::default()));
    let (_tx, rx) = mpsc::channel(1);
    let owner = client.clone();
    let format = serde_json::from_value(json!({"rate":24000,"width":2,"channels":1})).unwrap();
    let play = tokio::spawn(async move { owner.play(format, rx).await });
    assert_eq!(peer::receive(&mut stream).await.header.r#type, "audio-start");
    // Keep the peer alive but stop reading: audio-stop cannot fit. Paused time
    // advances only the operation deadline, with no wall-clock synchronization.
    assert_eq!(client.cut_with_stats().await, Err(crate::Error::Timeout));
    assert_eq!(play.await.unwrap(), Err(crate::Error::Timeout));
}

#[tokio::test]
async fn exact_cut_conserves_sent_and_dropped_and_requires_played() {
    let (io, mut stream) = duplex(256);
    let client = Arc::new(WyomingClient::from_io(
        io,
        ClientOptions {
            reply_capacity: 64,
            ..ClientOptions::default()
        },
    ));
    assert_eq!(client.cut_with_stats().await.unwrap(), CutStats::default());
    let (tx, rx) = mpsc::channel(64);
    for value in 0..50 {
        tx.send(vec![value; 40]).await.unwrap();
    }
    let owner = client.clone();
    let format = serde_json::from_value(json!({"rate":24000,"width":2,"channels":1})).unwrap();
    let play = tokio::spawn(async move { owner.play(format, rx).await });
    assert_eq!(peer::receive(&mut stream).await.header.r#type, "audio-start");
    let frame = peer::receive(&mut stream).await;
    assert_eq!(frame.payload, vec![0; 40]);
    let owner = client.clone();
    let cut = tokio::spawn(async move { owner.cut_with_stats().await });
    let mut sent = 1_u8;
    loop {
        let frame = peer::receive(&mut stream).await;
        if frame.header.r#type == "hassmic-flush" {
            break;
        }
        assert_eq!(frame.header.r#type, "audio-chunk");
        assert_eq!(frame.payload, vec![sent; 40]);
        sent += 1;
    }
    let stats = cut.await.unwrap().unwrap();
    assert_eq!(u64::from(sent) + stats.dropped_frames, 50);
    assert_eq!(client.cut_with_stats().await.unwrap(), CutStats::default());
    assert!(!play.is_finished(), "cut must not fabricate played");
    let describe = async {
        client.describe().await.unwrap();
    };
    let boundary = async {
        assert_eq!(peer::receive(&mut stream).await.header.r#type, "describe");
        peer::send(&mut stream, "info", json!({}), vec![]).await;
    };
    tokio::join!(describe, boundary);
    peer::send(&mut stream, "played", json!({}), vec![]).await;
    assert_eq!(play.await.unwrap(), Ok(PlayOutcome::Cut));
    assert_eq!(client.cut_with_stats().await.unwrap(), CutStats::default());
    assert!(tx.is_closed());
    client.close().await.unwrap();
}

async fn report_failure(client: &WyomingClient) {
    client
        .send_error("echo-unavailable", "Live unavailable.")
        .await
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn played_then_end_write_timeout_never_reports_run_success() {
    let (io, mut stream) = duplex(1);
    let mut client = WyomingClient::from_io(io, ClientOptions::default());
    let mut events = client.events().unwrap();
    peer::send(
        &mut stream,
        "run-pipeline",
        json!({"start_stage":"asr","end_stage":"tts",
        "restart_on_end":false,"wake_word_name":"Computer"}),
        vec![],
    )
    .await;
    let mut run = events.recv().await.unwrap().unwrap().run.unwrap();
    let (tx, rx) = mpsc::channel(1);
    drop(tx);
    let format = serde_json::from_value(json!({"rate":24000,"width":2,"channels":1})).unwrap();
    let play = client.play(format, rx);
    let server = async {
        assert_eq!(peer::receive(&mut stream).await.header.r#type, "audio-start");
        assert_eq!(peer::receive(&mut stream).await.header.r#type, "audio-stop");
        peer::send(&mut stream, "played", json!({}), vec![]).await;
    };
    let (result, ()) = tokio::join!(play, server);
    assert_eq!(result, Ok(PlayOutcome::Played));
    // Played succeeds independently; the stalled end command cannot close the
    // run successfully or fabricate successful capture EOF.
    assert_eq!(client.end_run().await, Err(Error::Timeout));
    assert!(matches!(run.recv().await, Err(Error::Timeout | Error::Closed)));
}

#[tokio::test(start_paused = true)]
async fn pause_and_close_end_active_reply_without_fabricating_played() {
    for pause in [true, false] {
        let (io, mut stream) = duplex(4096);
        let client = Arc::new(WyomingClient::from_io(io, ClientOptions::default()));
        let (_tx, rx) = mpsc::channel(1);
        let owner = client.clone();
        let format = serde_json::from_value(json!({"rate":24000,"width":2,"channels":1})).unwrap();
        let play = tokio::spawn(async move { owner.play(format, rx).await });
        assert_eq!(peer::receive(&mut stream).await.header.r#type, "audio-start");
        if pause {
            client.pause().await.unwrap();
        } else {
            client.close().await.unwrap();
        }
        assert_eq!(peer::receive(&mut stream).await.header.r#type, "hassmic-end");
        assert_eq!(play.await.unwrap(), Err(crate::Error::Closed));
        if pause {
            assert_eq!(peer::receive(&mut stream).await.header.r#type, "pause-satellite");
            client.close().await.unwrap();
        }
        assert_eq!(tokio::io::AsyncReadExt::read(&mut stream, &mut [0]).await.unwrap(), 0);
    }
}

#[tokio::test(start_paused = true)]
async fn cut_after_drain_before_played_preserves_capture() {
    cut_at_reply_end(true).await;
}

#[tokio::test(start_paused = true)]
async fn cut_at_last_chunk_preserves_capture() {
    cut_at_reply_end(false).await;
}

async fn cut_at_reply_end(drained: bool) {
    let (io, mut stream) = duplex(4096);
    let mut client = WyomingClient::from_io(io, ClientOptions::default());
    let mut events = client.events().unwrap();
    peer::send(
        &mut stream,
        "run-pipeline",
        json!({"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"Computer"}),
        vec![],
    )
    .await;
    let mut run = events.recv().await.unwrap().unwrap().run.unwrap();
    let (tx, rx) = mpsc::channel(1);
    tx.send(vec![1, 2]).await.unwrap();
    let mut tx = Some(tx);
    let format = serde_json::from_value(json!({"rate":24000,"width":2,"channels":1})).unwrap();
    let mut play = Box::pin(client.play(format, rx));
    let boundary = async {
        assert_eq!(peer::receive(&mut stream).await.header.r#type, "audio-start");
        assert_eq!(peer::receive(&mut stream).await.payload, vec![1, 2]);
        if drained {
            drop(tx.take());
            assert_eq!(peer::receive(&mut stream).await.header.r#type, "audio-stop");
        }
    };
    tokio::select! { biased; result = &mut play => unreachable!("unexpected {result:?}"), () = boundary => () }
    let caller = async {
        assert_eq!(client.cut_with_stats().await.unwrap(), CutStats::default());
        assert_eq!(client.cut_with_stats().await.unwrap(), CutStats::default());
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(play.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        client.describe().await.unwrap();
        assert_eq!(play.await, Ok(PlayOutcome::Cut));
        assert_eq!(run.recv().await.unwrap().unwrap().payload.as_ref(), vec![3, 4]);
        client.close().await.unwrap();
    };
    let server = async {
        assert_eq!(peer::receive(&mut stream).await.header.r#type, "hassmic-flush");
        // The describe barrier also proves the repeated cut emitted no frames.
        assert_eq!(peer::receive(&mut stream).await.header.r#type, "describe");
        peer::send(&mut stream, "info", json!({}), vec![]).await;
        peer::send(&mut stream, "played", json!({}), vec![]).await;
        peer::send(
            &mut stream,
            "audio-chunk",
            json!({"rate":16000,"width":2,"channels":1}),
            vec![3, 4],
        )
        .await;
        // Played retired the cut reply, but the run still owns capture:
        // closing it ends the device run explicitly.
        assert_eq!(peer::receive(&mut stream).await.header.r#type, "hassmic-end");
        assert_eq!(tokio::io::AsyncReadExt::read(&mut stream, &mut [0]).await.unwrap(), 0);
    };
    tokio::join!(caller, server);
}
