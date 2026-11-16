//! T1: public cut boundary over in-memory duplex, with no sockets or clock sleeps.
use crate::{ClientOptions, CutStats, PlayOutcome, WyomingClient, wyoming_test_support as peer};
use serde_json::json;
use std::sync::Arc;
use tokio::{io::duplex, sync::mpsc};

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
