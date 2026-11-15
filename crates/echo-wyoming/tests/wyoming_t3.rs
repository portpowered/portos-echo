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
    let (client, peer) = pair().await;
    wyoming_control::cancel_play(client, peer).await;
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

#[tokio::test]
#[ignore = "T3 loopback: run with make test-sim"]
async fn t3_fake_malformed_known_event_closes_generation() {
    let (client, peer) = pair().await;
    wyoming_peer::malformed(client, peer).await;
}
