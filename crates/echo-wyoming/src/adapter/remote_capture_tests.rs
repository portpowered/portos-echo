//! T1 checks the device wire and link events with an in-memory peer.
use super::*;
use crate::wyoming_test_support::{receive, send};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::{
    io::{DuplexStream, duplex},
    time::{Instant, advance},
};

const WINDOW: Duration = Duration::from_secs(2);
const FIRST: &str = "00000000-0000-4000-8000-000000000001";
const SECOND: &str = "00000000-0000-4000-8000-000000000002";

async fn request(client: &WyomingClient, peer: &mut DuplexStream, request_id: &Arc<str>) {
    let deadline = (Instant::now() + WINDOW).into_std();
    EchoLink::request_remote_capture(client, Arc::downgrade(request_id), deadline)
        .await
        .unwrap();
    let frame = receive(peer).await;
    assert_eq!(
        frame.typed().unwrap(),
        WyomingEvent::Other("hassmic-start-run".into(), json!({"requestId": request_id.as_ref()}))
    );
}

async fn pipeline(peer: &mut DuplexStream, request_id: Option<Value>) {
    let mut data = json!({
        "start_stage": "asr", "end_stage": "tts", "restart_on_end": false,
        "wake_word_name": "Computer"
    });
    if let Some(request_id) = request_id {
        data["requestId"] = request_id;
        data["wake_word_name"] = json!("");
    }
    send(peer, "run-pipeline", data, vec![]).await;
}

async fn setup() -> (WyomingClient, Events, DuplexStream) {
    let (io, mut peer) = duplex(65536);
    let mut client = WyomingClient::from_io(io, ClientOptions::default());
    let events = EchoLink::events(&mut client).unwrap();
    client.enable_capture(30).await.unwrap();
    assert_eq!(receive(&mut peer).await.header.r#type, "run-satellite");
    (client, events, peer)
}

#[tokio::test(start_paused = true)]
async fn remote_capture_requires_live_id_and_preserves_independent_wake() {
    let (client, mut events, mut peer) = setup().await;
    let first: Arc<str> = FIRST.into();
    request(&client, &mut peer, &first).await;
    advance(WINDOW).await;
    pipeline(&mut peer, Some(json!(FIRST))).await;
    assert!(matches!(
        events.recv().await.unwrap(),
        Some(LinkEvent::StaleRemoteCapture)
    ));
    send(
        &mut peer,
        "audio-chunk",
        json!({"rate":16000,"width":2,"channels":1}),
        vec![7, 0],
    )
    .await;
    assert!(matches!(
        events.recv().await.unwrap(),
        Some(LinkEvent::MicrophoneDropped(1))
    ));

    let second: Arc<str> = SECOND.into();
    request(&client, &mut peer, &second).await;
    pipeline(&mut peer, None).await;
    let Some(LinkEvent::Wake(local)) = events.recv().await.unwrap() else {
        unreachable!()
    };
    assert_eq!(local.wake_phrase(), "Computer");
    drop(local);
    for stale in [json!(FIRST), Value::Null] {
        pipeline(&mut peer, Some(stale)).await;
        assert!(matches!(
            events.recv().await.unwrap(),
            Some(LinkEvent::StaleRemoteCapture)
        ));
    }
    pipeline(&mut peer, Some(json!(SECOND))).await;
    let Some(LinkEvent::RemoteCapture { request_id, mut run }) = events.recv().await.unwrap() else {
        unreachable!()
    };
    assert_eq!(request_id, SECOND);
    send(
        &mut peer,
        "audio-chunk",
        json!({"rate":16000,"width":2,"channels":1}),
        vec![8, 0],
    )
    .await;
    assert_eq!(run.recv().await.unwrap().unwrap().payload.as_ref(), [8, 0]);
}

#[tokio::test(start_paused = true)]
async fn dropping_remote_capture_reservation_discards_an_in_window_response() {
    let (client, mut events, mut peer) = setup().await;
    let id: Arc<str> = FIRST.into();
    request(&client, &mut peer, &id).await;
    drop(id);
    pipeline(&mut peer, Some(json!(FIRST))).await;
    assert!(matches!(
        events.recv().await.unwrap(),
        Some(LinkEvent::StaleRemoteCapture)
    ));
    EchoLink::end_run(&client).await.unwrap();
    let mut no_end_frame = std::pin::pin!(receive(&mut peer));
    std::future::poll_fn(|cx| {
        assert!(no_end_frame.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
}
