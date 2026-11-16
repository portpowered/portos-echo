//! T1: public cut boundary over in-memory duplex, with no sockets or clock sleeps.
use crate::{ClientOptions, CutStats, PlayOutcome, WyomingClient, wyoming_test_support as peer};
use serde_json::json;
use std::sync::Arc;
use tokio::{io::duplex, sync::mpsc};

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
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut play)
            .await
            .is_err()
    );
    drop(play);
    let caller = async {
        client
            .send_error("echo-unavailable", "Live unavailable.")
            .await
            .unwrap();
        let mut run = events.recv().await.unwrap().unwrap().run.unwrap();
        assert_eq!(run.recv().await.unwrap().unwrap().payload, vec![1, 2]);
        let (tx, rx) = mpsc::channel(1);
        tx.send(vec![3, 4]).await.unwrap();
        drop(tx);
        let format = serde_json::from_value(json!({"rate":24000,"width":2,"channels":1})).unwrap();
        assert_eq!(client.play(format, rx).await, Ok(PlayOutcome::Played));
        client.close().await.unwrap();
    };
    let server = async {
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
    assert_eq!(peer::receive(&mut stream).await.header.r#type, "audio-stop");
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
        if frame.header.r#type == "audio-stop" {
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
