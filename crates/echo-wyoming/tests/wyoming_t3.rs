#![cfg(test)]
//! T3 component tests: controlled peers on 127.0.0.1 only, no external services.
#[cfg(test)]
mod wyoming_peer {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/wyoming_test_support.rs"));
}

use echo_wyoming::{ClientOptions, WyomingClient};
use tokio::net::{TcpListener, TcpStream};

mod wyoming_control {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/wyoming_session/wyoming_control_support.rs"
    ));
}

async fn bounded_pair() -> (WyomingClient, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let opts = ClientOptions {
        mic_capacity: 2,
        reply_capacity: 2,
        ..ClientOptions::default()
    };
    let (client, server) = tokio::join!(
        WyomingClient::connect(listener.local_addr().unwrap(), opts),
        listener.accept()
    );
    (client.unwrap(), server.unwrap().0)
}

#[tokio::test]
#[ignore = "T3 loopback: run with make test-sim"]
async fn t3_fake_cut_discards_unsent_and_preserves_duplex() {
    let (client, peer) = bounded_pair().await;
    wyoming_control::cut_duplex(client, peer).await;
}

#[tokio::test]
#[ignore = "T3 loopback: run with make test-sim"]
async fn t3_fake_pause_resume_capture() {
    let (client, peer) = pair().await;
    wyoming_control::pause_resume(client, peer).await;
}

#[tokio::test]
#[ignore = "T3 loopback: run with make test-sim"]
async fn t3_fake_cancelled_play_closes_generation() {
    use serde_json::json;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut client = WyomingClient::connect(listener.local_addr().unwrap(), ClientOptions::default())
        .await
        .unwrap();
    let (mut peer, _) = listener.accept().await.unwrap();
    let mut events = client.events().unwrap();
    let caller = async {
        let (_tx, rx) = tokio::sync::mpsc::channel(1);
        let format: echo_contracts::wyoming::AudioFormat =
            serde_json::from_value(json!({"rate":24000,"width":2,"channels":1})).unwrap();
        let mut play = Box::pin(client.play(format.clone(), rx));
        let mut next = Box::pin(events.recv());
        let event = std::future::poll_fn(|cx| {
            assert!(play.as_mut().poll(cx).is_pending());
            next.as_mut().poll(cx)
        })
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(event.event, echo_contracts::wyoming::WyomingEvent::Ping(_)));
        drop(next);
        drop(play);
        assert!(matches!(
            events.recv().await.unwrap().unwrap().event,
            echo_contracts::wyoming::WyomingEvent::Played(_)
        ));
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        drop(tx);
        assert_eq!(client.play(format, rx).await, Ok(echo_wyoming::PlayOutcome::Played));
        client.close().await.unwrap();
    };
    let server = async {
        assert_eq!(wyoming_peer::receive(&mut peer).await.header.r#type, "audio-start");
        wyoming_peer::send(&mut peer, "ping", json!(null), vec![]).await;
        assert_eq!(tokio::io::AsyncReadExt::read(&mut peer, &mut [0]).await.unwrap(), 0);
        let (mut next, _) = listener.accept().await.unwrap();
        wyoming_peer::send(&mut next, "played", json!(null), vec![]).await;
        assert_eq!(wyoming_peer::receive(&mut next).await.header.r#type, "audio-start");
        assert_eq!(wyoming_peer::receive(&mut next).await.header.r#type, "audio-stop");
        wyoming_peer::send(&mut next, "played", json!(null), vec![]).await;
        assert_eq!(tokio::io::AsyncReadExt::read(&mut next, &mut [0]).await.unwrap(), 0);
    };
    tokio::join!(caller, server);
}

async fn pair() -> (WyomingClient, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (client, server) = tokio::join!(
        WyomingClient::connect(listener.local_addr().unwrap(), ClientOptions::default()),
        listener.accept()
    );
    (client.unwrap(), server.unwrap().0)
}

#[tokio::test]
#[ignore = "T3 loopback: run with make test-sim"]
async fn t3_fake_typed_runs_without_audio_start() {
    let (client, peer) = pair().await;
    wyoming_peer::microphone(client, peer).await;
}

#[tokio::test]
#[ignore = "T3 loopback: run with make test-sim"]
async fn t3_fake_empty_minimum_ten_chunks_wait_for_played() {
    let (client, peer) = pair().await;
    wyoming_peer::playback(client, peer).await;
}

#[path = "wyoming_host/wyoming_host.rs"]
mod wyoming_host;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "T3 prebuilt pinned host: run with make test-sim"]
async fn t3_host_lifecycle_restart() {
    wyoming_host::lifecycle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "T3 prebuilt pinned host: run with make test-sim"]
async fn t3_host_pause_resume_cut_duplex() {
    wyoming_host::controls().await;
}

#[tokio::test]
#[ignore = "T3 loopback: run with make test-sim"]
async fn t3_fake_malformed_known_event_closes_generation() {
    use serde_json::json;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut client = WyomingClient::connect(listener.local_addr().unwrap(), ClientOptions::default())
        .await
        .unwrap();
    let (mut server, _) = listener.accept().await.unwrap();
    let mut events = client.events().unwrap();
    let caller = async {
        client.run_satellite().await.unwrap();
        let mut run = events.recv().await.unwrap().unwrap().run.unwrap();
        assert_eq!(run.recv().await, Err(echo_wyoming::Error::Protocol));
        let fresh = events.recv().await.unwrap().unwrap();
        assert_eq!(fresh.event, echo_contracts::wyoming::WyomingEvent::Ping(json!("fresh")));
        client.close().await.unwrap();
    };
    let server = async {
        assert_eq!(wyoming_peer::receive(&mut server).await.header.r#type, "run-satellite");
        wyoming_peer::send(
            &mut server,
            "run-pipeline",
            json!({"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"alexa"}),
            vec![],
        )
        .await;
        wyoming_peer::send(&mut server, "audio-chunk", json!({}), vec![9]).await;
        assert_eq!(tokio::io::AsyncReadExt::read(&mut server, &mut [0]).await.unwrap(), 0);
        let (mut next, _) = listener.accept().await.unwrap();
        assert_eq!(wyoming_peer::receive(&mut next).await.header.r#type, "run-satellite");
        wyoming_peer::send(&mut next, "ping", json!("fresh"), vec![]).await;
        assert_eq!(tokio::io::AsyncReadExt::read(&mut next, &mut [0]).await.unwrap(), 0);
    };
    tokio::join!(caller, server);
}

#[test]
#[ignore = "T3 prebuilt pinned host: run with make test-sim"]
fn t3_host_failed_startup_reaps_process_and_temp_files() {
    wyoming_host::failed_startup_cleanup();
}
