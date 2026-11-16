//! T1 transport adapter checks through in-memory byte streams, without sockets.
use super::*;
use crate::wyoming_test_support::send;
use serde_json::json;
use std::{
    future::Future,
    task::{Context, Waker},
};
use tokio::io::duplex;

#[tokio::test(start_paused = true)]
async fn mute_orders_snapshot_frames_eof_and_cancellation_safe_event_delivery() {
    let (client, mut events, mut peer) = setup();
    let server = muted_run_peer(&mut peer);
    tokio::pin!(server);
    let caller = async {
        assert!(matches!(events.recv().await.unwrap(), Some(LinkEvent::Mute(true))));
        assert!(matches!(events.recv().await.unwrap(), Some(LinkEvent::Mute(false))));
        let Some(LinkEvent::Wake(mut run)) = events.recv().await.unwrap() else {
            unreachable!()
        };
        // A response after the peer's full sequence proves both pre-mute
        // frames are coalesced before receiving their notification.
        client.describe().await.unwrap();
        assert!(matches!(events.recv().await.unwrap(), Some(LinkEvent::Other)));
        let mut mute = Box::pin(events.recv());
        assert!(mute.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending());
        drop(mute); // cancellation must not lose the pending Mute.
        for n in 0..2 {
            assert_eq!(MicrophoneRun::recv(&mut run).await.unwrap().unwrap().payload, [n, 0]);
        }
        assert_eq!(MicrophoneRun::recv(&mut run).await.unwrap(), None);
        assert!(matches!(events.recv().await.unwrap(), Some(LinkEvent::Mute(true))));
        // Drain all post-mute notifications, so counts are measured after their routing.
        assert!(matches!(
            events.recv().await.unwrap(),
            Some(LinkEvent::MicrophoneDropped(3))
        ));
        assert!(matches!(events.recv().await.unwrap(), Some(LinkEvent::Other)));
        assert_eq!(MicrophoneRun::recv(&mut run).await.unwrap(), None);
        assert_eq!(
            MicrophoneRun::stats(&run),
            RunStats {
                received: 5,
                dropped: 3
            }
        );
    };
    tokio::select! { biased; () = caller => (), () = &mut server => unreachable!() }
    EchoLink::close(&client).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn malformed_mute_is_a_protocol_failure_and_info_stays_other() {
    for data in [json!({}), json!({"muted":"false"}), json!({"muted":null})] {
        let (io, mut peer) = duplex(65536);
        let mut client = WyomingClient::from_io(io, ClientOptions::default());
        let mut events = EchoLink::events(&mut client).unwrap();
        send(&mut peer, "hassmic-mute", data, vec![]).await;
        assert!(matches!(events.recv().await, Err(Error::Protocol)));
    }
}

fn setup() -> (WyomingClient, Events, tokio::io::DuplexStream) {
    let (io, peer) = duplex(65536);
    let mut client = WyomingClient::from_io(io, ClientOptions::default());
    let events = EchoLink::events(&mut client).unwrap();
    (client, events, peer)
}

#[tokio::test(start_paused = true)]
async fn suppressed_idle_and_ended_run_chunks_keep_wire_order_without_audio() {
    let (client, mut events, mut peer) = setup();
    let frames = [
        ("audio-chunk", json!({"rate":16000,"width":2,"channels":1}), vec![1, 0]),
        ("hassmic-mute", json!({"muted":true}), vec![]),
        ("audio-chunk", json!({"rate":16000,"width":2,"channels":1}), vec![2, 0]),
        ("hassmic-mute", json!({"muted":false}), vec![]),
        ("ping", json!({}), vec![]),
        ("audio-chunk", json!({"rate":16000,"width":2,"channels":1}), vec![3, 0]),
    ];
    for (name, data, payload) in frames {
        send(&mut peer, name, data, payload).await;
    }
    assert!(matches!(
        events.recv().await.unwrap(),
        Some(LinkEvent::MicrophoneDropped(1))
    ));
    assert!(matches!(events.recv().await.unwrap(), Some(LinkEvent::Mute(true))));
    assert!(matches!(
        events.recv().await.unwrap(),
        Some(LinkEvent::MicrophoneDropped(1))
    ));
    assert!(matches!(events.recv().await.unwrap(), Some(LinkEvent::Mute(false))));
    assert!(matches!(events.recv().await.unwrap(), Some(LinkEvent::Other)));
    assert!(matches!(
        events.recv().await.unwrap(),
        Some(LinkEvent::MicrophoneDropped(1))
    ));
    EchoLink::close(&client).await.unwrap();
}

async fn muted_run_peer(peer: &mut tokio::io::DuplexStream) {
    send(peer, "hassmic-mute", json!({"muted":true}), vec![]).await;
    send(peer, "hassmic-mute", json!({"muted":false}), vec![]).await;
    send(
        peer,
        "run-pipeline",
        json!({"start_stage":"asr","end_stage":"tts",
            "restart_on_end":false,"wake_word_name":"Computer"}),
        vec![],
    )
    .await;
    for n in 0..5 {
        if n == 2 {
            send(peer, "hassmic-mute", json!({"muted":true}), vec![]).await;
        }
        send(
            peer,
            "audio-chunk",
            json!({"rate":16000,"width":2,"channels":1}),
            vec![n, 0],
        )
        .await;
    }
    send(peer, "ping", json!({}), vec![]).await;
    assert_eq!(
        crate::wyoming_test_support::receive(peer).await.header.r#type,
        "describe"
    );
    send(peer, "info", json!({}), vec![]).await;
    std::future::pending::<()>().await;
}
