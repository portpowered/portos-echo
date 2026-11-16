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
        Self::bind_frames(3).await
    }

    pub async fn bind_frames(frames: u16) -> Self {
        Self::bind_after(frames, None).await
    }

    pub async fn bind_after(
        frames: u16,
        mut gate: Option<(tokio::sync::oneshot::Sender<()>, tokio::sync::oneshot::Receiver<()>)>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = "http://127.0.0.1";
        let url = format!("{origin}:{}", listener.local_addr().unwrap().port());
        let (events, mut receive) = mpsc::channel::<String>(8);
        let task = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap().0;
            loop {
                stream = serve(&listener, stream, &mut receive, frames, gate.take()).await;
            }
        });
        Self { url, events, task }
    }
}

async fn serve(
    listener: &TcpListener,
    mut stream: TcpStream,
    receive: &mut mpsc::Receiver<String>,
    frames: u16,
    gate: Option<(tokio::sync::oneshot::Sender<()>, tokio::sync::oneshot::Receiver<()>)>,
) -> TcpStream {
    let body = request(&mut stream).await;
    if let Some((ready, release)) = gate {
        ready.send(()).unwrap();
        release.await.unwrap();
    }
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
    short_audio(&mut peer, frames).await;
    next_connection(listener, receive, &mut peer).await
}

async fn next_connection(
    listener: &TcpListener,
    receive: &mut mpsc::Receiver<String>,
    peer: &mut LivePeer,
) -> TcpStream {
    loop {
        tokio::select! {
            next = listener.accept() => return next.unwrap().0,
            event = receive.recv() => match event {
                Some(event) => peer.send_text(&event).await.unwrap(),
                None => return listener.accept().await.unwrap().0,
            },
            result = drive(peer) => if result.is_err() { return listener.accept().await.unwrap().0; },
        }
        while peer.pop_audio().is_some() {}
    }
}

async fn short_audio(peer: &mut LivePeer, frames: u16) {
    let mut encoder = echo_audio::OpusEncoder::new().unwrap();
    let mut bytes = [0; 1275];
    for sequence in 0..frames {
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
