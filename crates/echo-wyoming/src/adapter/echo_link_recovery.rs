use super::*;
use crate::wyoming_test_support::{receive, send};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

#[tokio::test(start_paused = true)]
async fn echo_link_queued_microphone_drains_before_transport_or_protocol_failure() {
    for malformed in [false, true] {
        let (io, mut peer) = duplex(65536);
        let mut client = WyomingClient::from_io(io, ClientOptions::default());
        let mut events = EchoLink::events(&mut client).unwrap();
        let caller = async {
            let Some(LinkEvent::Wake(mut run)) = LinkEvents::recv(&mut events).await.unwrap() else {
                unreachable!()
            };
            assert_eq!(run.wake_phrase(), "Computer");
            assert_eq!(MicrophoneRun::recv(&mut run).await.unwrap().unwrap().payload, [1, 2]);
            assert_eq!(
                MicrophoneRun::stats(&run),
                RunStats {
                    received: 1,
                    dropped: 0
                }
            );
            let expected = if malformed { Error::Protocol } else { Error::Transport };
            assert_eq!(MicrophoneRun::recv(&mut run).await, Err(expected));
            assert!(matches!(
                LinkEvents::recv(&mut events).await.unwrap(),
                Some(LinkEvent::Other)
            ));
            assert!(matches!(LinkEvents::recv(&mut events).await, Err(error) if error == expected));
        };
        let server = async {
            send(
                &mut peer,
                "run-pipeline",
                json!({"start_stage":"asr","end_stage":"tts",
                "restart_on_end":false,"wake_word_name":"Computer"}),
                vec![],
            )
            .await;
            send(
                &mut peer,
                "audio-chunk",
                json!({"rate":16000,"width":2,"channels":1}),
                vec![1, 2],
            )
            .await;
            if malformed {
                peer.write_all(b"{broken}\n").await.unwrap();
            }
            drop(peer);
        };
        tokio::join!(caller, server);
    }
}

#[tokio::test(start_paused = true)]
async fn echo_link_paused_reconnect_stays_paused_then_enabled_reconnect_registers_once() {
    let (io, peer) = duplex(65536);
    let (connections, receiver) = mpsc::channel(2);
    let receiver = std::sync::Arc::new(tokio::sync::Mutex::new(receiver));
    let mut client = WyomingClient::reconnecting(io, ClientOptions::default(), move || {
        let receiver = receiver.clone();
        async move { receiver.lock().await.recv().await.unwrap() }
    });
    let mut events = EchoLink::events(&mut client).unwrap();
    let caller = async {
        client.enable_capture().await.unwrap();
        EchoLink::pause(&client).await.unwrap();
        // Fresh peer readiness arrives through the same retained receiver.
        assert!(matches!(
            LinkEvents::recv(&mut events).await.unwrap(),
            Some(LinkEvent::Other)
        ));
        client.probe().await.unwrap();
        assert!(matches!(
            LinkEvents::recv(&mut events).await.unwrap(),
            Some(LinkEvent::Other)
        ));
        client.enable_capture().await.unwrap();
        client.enable_capture().await.unwrap();
        assert!(matches!(
            LinkEvents::recv(&mut events).await.unwrap(),
            Some(LinkEvent::Other)
        ));
        EchoLink::close(&client).await.unwrap();
        assert!(LinkEvents::recv(&mut events).await.unwrap().is_none());
    };
    tokio::join!(caller, reconnect_server(peer, connections));
}

async fn reconnect_server(
    mut peer: tokio::io::DuplexStream,
    connections: mpsc::Sender<std::io::Result<tokio::io::DuplexStream>>,
) {
    kind(&mut peer, "run-satellite").await;
    kind(&mut peer, "pause-satellite").await;
    drop(peer);
    let (io, mut peer) = duplex(65536);
    connections.send(Ok(io)).await.unwrap();
    send(&mut peer, "ping", json!({}), vec![]).await;
    kind(&mut peer, "describe").await;
    send(&mut peer, "info", json!({}), vec![]).await;
    kind(&mut peer, "run-satellite").await;
    drop(peer);
    let (io, mut peer) = duplex(65536);
    connections.send(Ok(io)).await.unwrap();
    kind(&mut peer, "run-satellite").await;
    send(&mut peer, "ping", json!({}), vec![]).await;
    assert_eq!(peer.read(&mut [0]).await.unwrap(), 0);
}

async fn kind(peer: &mut tokio::io::DuplexStream, expected: &str) {
    assert_eq!(receive(peer).await.header.r#type, expected);
}
