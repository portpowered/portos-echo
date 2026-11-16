//! A controlled synthetic Live boundary, using the shipped public peer.
use super::*;
#[cfg(unix)]
#[path = "../bridge_muse_tests/mod.rs"]
mod bridge_muse_tests;
use echo_live::LivePeer;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::JoinHandle,
};

pub struct Live {
    pub url: String,
    pub events: mpsc::Sender<String>,
    pub commentary: mpsc::Receiver<String>,
    pub microphone_frames: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    task: JoinHandle<()>,
}

// Immutable packets are local to one fixture. Every call still offers the
// same freshly encoded sequence; preparation is paid once, outside each cut.
struct Audio {
    initial: Vec<Vec<u8>>,
    more: Vec<Vec<u8>>,
    muse: Vec<Vec<u8>>,
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
        Self::bind_listener(listener, frames, gate.take())
    }

    pub(super) fn bind_listener(
        listener: TcpListener,
        frames: u16,
        mut gate: Option<(tokio::sync::oneshot::Sender<()>, tokio::sync::oneshot::Receiver<()>)>,
    ) -> Self {
        let origin = "http://127.0.0.1";
        let url = format!("{origin}:{}", listener.local_addr().unwrap().port());
        let (events, mut receive) = mpsc::channel::<String>(8);
        let (sent, commentary) = mpsc::channel(8);
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        let microphone_frames = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let microphone = microphone_frames.clone();
        let audio = Audio {
            initial: encode(frames),
            more: encode(32),
            muse: encode(10),
        };
        let task = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap().0;
            loop {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                stream = serve(&listener, stream, &mut receive, &audio, gate.take(), &sent, &microphone).await;
            }
        });
        Self {
            url,
            events,
            commentary,
            microphone_frames,
            calls,
            task,
        }
    }
}

async fn serve(
    listener: &TcpListener,
    mut stream: TcpStream,
    receive: &mut mpsc::Receiver<String>,
    audio: &Audio,
    gate: Option<(tokio::sync::oneshot::Sender<()>, tokio::sync::oneshot::Receiver<()>)>,
    commentary: &mpsc::Sender<String>,
    microphone: &std::sync::atomic::AtomicUsize,
) -> TcpStream {
    let body = request(&mut stream).await;
    if let Some((ready, release)) = gate {
        ready.send(()).unwrap();
        release.await.unwrap();
    }
    let mut peer = Box::pin(LivePeer::bind(
        ([127, 0, 0, 1], 0).into(),
        tokio::time::Instant::now().into_std(),
    ))
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
    let mut sequence = 0;
    short_audio(&mut peer, &audio.initial, &mut sequence).await;
    next_connection(
        listener,
        receive,
        &mut peer,
        &mut sequence,
        commentary,
        microphone,
        audio,
    )
    .await
}

async fn next_connection(
    listener: &TcpListener,
    receive: &mut mpsc::Receiver<String>,
    peer: &mut LivePeer,
    sequence: &mut u16,
    commentary: &mpsc::Sender<String>,
    microphone: &std::sync::atomic::AtomicUsize,
    audio: &Audio,
) -> TcpStream {
    loop {
        tokio::select! {
            next = listener.accept() => return next.unwrap().0,
            event = receive.recv() => match event {
                Some(event) => send_event(peer, &event, audio, sequence).await,
                None => return listener.accept().await.unwrap().0,
            },
            result = drive(peer) => if result.is_err() { return listener.accept().await.unwrap().0; },
        }
        while peer.pop_audio().is_some() {
            microphone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        while let Some(text) = peer.pop_text() {
            commentary.try_send(text).unwrap();
        }
    }
}

async fn send_event(peer: &mut LivePeer, event: &str, audio: &Audio, sequence: &mut u16) {
    match event {
        "synthetic.more_audio" => short_audio(peer, &audio.more, sequence).await,
        "synthetic.muse_audio" => short_audio(peer, &audio.muse, sequence).await,
        _ => peer.send_text(event).await.unwrap(),
    }
}

fn encode(frames: u16) -> Vec<Vec<u8>> {
    let mut encoder = echo_audio::OpusEncoder::new().unwrap();
    let mut bytes = [0; 1275];
    (0..frames)
        .map(|_| {
            let size = encoder.encode(&[0; 960], &mut bytes).unwrap();
            bytes[..size].to_vec()
        })
        .collect()
}

async fn short_audio(peer: &mut LivePeer, audio: &[Vec<u8>], sequence: &mut u16) {
    for payload in audio {
        peer.send_audio(
            echo_live::AudioFrame {
                sequence: *sequence,
                timestamp: u32::from(*sequence) * 960,
                payload: payload.clone(),
            },
            tokio::time::Instant::now().into_std(),
        )
        .await
        .unwrap();
        *sequence += 1;
    }
}

async fn drive(peer: &mut LivePeer) -> Result<(), echo_live::Error> {
    let deadline = peer.flush().await?;
    tokio::select! {
        result = peer.readable() => { result?; peer.receive(tokio::time::Instant::now().into_std()).await?; },
        () = tokio::time::sleep_until(deadline.into()) => peer.timeout(tokio::time::Instant::now().into_std()).await?,
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
