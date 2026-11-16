//! A controlled synthetic Live boundary, using the shipped public peer.
use super::*;
use echo_live::LivePeer;
use std::time::Instant;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::JoinHandle,
};

pub struct Live {
    pub url: String,
    pub events: mpsc::Sender<String>,
    task: JoinHandle<()>,
}

impl Drop for Live {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Live {
    pub async fn bind() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = "http://127.0.0.1";
        let url = format!("{origin}:{}", listener.local_addr().unwrap().port());
        let (events, mut receive) = mpsc::channel::<String>(8);
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let body = request(&mut stream).await;
            let mut peer = Box::pin(LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now()))
                .await
                .unwrap();
            let answer = peer.answer(body["sdp"].as_str().unwrap()).await.unwrap();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            drop(stream);
            while !peer.ready() {
                drive(&mut peer).await.unwrap();
            }
            short_audio(&mut peer).await;
            loop {
                tokio::select! {
                    event = receive.recv() => match event {
                        Some(event) => peer.send_text(&event).await.unwrap(),
                        None => break,
                    },
                    result = drive(&mut peer) => if result.is_err() { break; },
                }
                while peer.pop_audio().is_some() {}
            }
        });
        Self { url, events, task }
    }
}

async fn short_audio(peer: &mut LivePeer) {
    let mut encoder = echo_audio::OpusEncoder::new().unwrap();
    let mut bytes = [0; 1275];
    for sequence in 0..3 {
        let size = encoder.encode(&[0; 960], &mut bytes).unwrap();
        peer.send_audio(
            echo_live::AudioFrame {
                sequence,
                timestamp: u32::from(sequence) * 960,
                payload: bytes[..size].to_vec(),
            },
            Instant::now(),
        )
        .await
        .unwrap();
    }
}

async fn drive(peer: &mut LivePeer) -> Result<(), echo_live::Error> {
    let deadline = peer.flush().await?;
    tokio::select! {
        result = peer.readable() => { result?; peer.receive(Instant::now()).await?; },
        () = tokio::time::sleep_until(deadline.into()) => peer.timeout(Instant::now()).await?,
    }
    Ok(())
}

async fn request(stream: &mut TcpStream) -> Value {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        assert!(header.len() < 8192);
        header.push(stream.read_u8().await.unwrap());
    }
    let header = String::from_utf8(header).unwrap();
    let length: usize = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap();
    assert!(length < 16384);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}
