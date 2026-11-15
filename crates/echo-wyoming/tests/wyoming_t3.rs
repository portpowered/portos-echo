#![cfg(test)]
//! T3 component tests: controlled peers on 127.0.0.1 only, no external services.
#[path = "support/wyoming_peer.rs"]
mod wyoming_peer;

use echo_wyoming::{ClientOptions, WyomingClient};
use tokio::net::{TcpListener, TcpStream};

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
