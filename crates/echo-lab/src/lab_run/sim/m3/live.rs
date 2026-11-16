//! Lane-local controlled provider using the existing public WebRTC boundary.
use super::wire::Ledger;
use echo_live::{AudioFrame, LivePeer};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{oneshot, watch},
    task::JoinHandle,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Default)]
pub struct Received {
    pub frames: u64,
    pub first: bool,
    pub second: bool,
    pub transcript: Option<String>,
    pub failure: Option<String>,
}

pub struct Live {
    pub origin: String,
    pub observed: watch::Sender<Received>,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), String>>,
    closing: Arc<AtomicBool>,
}
impl Live {
    pub async fn bind(wire: watch::Sender<Ledger>) -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| "M3 Live: bind failed")?;
        let origin = format!(
            "http://{}",
            listener.local_addr().map_err(|_| "M3 Live: address failed")?
        );
        let (observed, _) = watch::channel(Received::default());
        let sender = observed.clone();
        let (stop, stopped) = oneshot::channel();
        let closing = Arc::new(AtomicBool::new(false));
        let owner_closing = closing.clone();
        let task = tokio::spawn(async move {
            let result = tokio::select! {
                result = session(listener, wire, &sender) => result,
                _ = stopped => Ok(()),
            };
            if let Err(error) = &result {
                if owner_closing.load(Ordering::Acquire) {
                    return Ok(());
                }
                sender.send_modify(|state| state.failure = Some(error.clone()));
            }
            result
        });
        Ok(Self {
            origin,
            observed,
            stop: Some(stop),
            task,
            closing,
        })
    }
    pub async fn close(mut self) -> Result<(), String> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        (&mut self.task).await.map_err(|_| "cleanup: M3 Live task failed")?
    }
    pub fn prepare_close(&self) {
        self.closing.store(true, Ordering::Release);
    }
}
impl Drop for Live {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn session(
    listener: TcpListener,
    wire: watch::Sender<Ledger>,
    observed: &watch::Sender<Received>,
) -> Result<(), String> {
    let (mut socket, _) = listener
        .accept()
        .await
        .map_err(|_| "M3 Live: signaling accept failed")?;
    drop(listener);
    let request = request(&mut socket).await?;
    let offer = request["sdp"].as_str().ok_or("M3 Live: missing offer")?;
    let mut peer = Box::pin(LivePeer::bind(
        "127.0.0.1:0".parse().map_err(|_| "M3 Live: address")?,
        Instant::now().into_std(),
    ))
    .await
    .map_err(|_| "M3 Live: peer bind failed")?;
    let answer = peer.answer(offer).await.map_err(|_| "M3 Live: invalid offer")?;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
        answer.len()
    );
    socket
        .write_all(response.as_bytes())
        .await
        .map_err(|_| "M3 Live: signaling write failed")?;
    drop(socket);
    replay(&mut peer, wire, observed).await
}

async fn replay(
    peer: &mut LivePeer,
    wire: watch::Sender<Ledger>,
    observed: &watch::Sender<Received>,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut frames = 0;
    let mut phase = 0;
    let mut sequence = 0;
    loop {
        if let Err(error) = drive(peer, deadline).await {
            if phase == 3 && wire.borrow().played.len() == 2 {
                return Ok(());
            }
            return Err(error);
        }
        while peer.pop_text().is_some() {}
        while peer.pop_audio().is_some() {
            frames += 1;
        }
        observed.send_modify(|state| state.frames = frames);
        let state = wire.borrow().clone();
        if phase == 0 && frames >= 44 && state.inputs == 1 {
            answer(peer, &mut sequence, 120, "The time is twelve.").await?;
            observed.send_modify(|state| {
                state.first = true;
                state.transcript = Some("The time is twelve.".into());
            });
            phase = 1;
        }
        if phase == 1 && frames >= 49 && state.inputs == 2 {
            if state.stops != 0 || !state.played.is_empty() {
                return Err("M3 overlap: interrupting input arrived after first reply closed".into());
            }
            peer.send_text(&json!({"type":"response.done","response":{"id":"old","status":"cancelled"}}).to_string())
                .await
                .map_err(|_| "M3 Live: cancel send failed")?;
            wire.send_modify(|state| state.cancel = Some(Instant::now()));
            phase = 2;
        }
        if phase == 2 && frames >= 50 && state.played.len() == 1 && state.old_sink_pcm.is_some() {
            answer(peer, &mut sequence, 10, "The day is Monday.").await?;
            peer.send_text(r#"{"type":"response.audio.done"}"#)
                .await
                .map_err(|_| "M3 Live: audio completion failed")?;
            observed.send_modify(|state| {
                state.second = true;
                state.transcript = Some("The day is Monday.".into());
            });
            phase = 3;
        }
        if phase == 3 && state.played.len() == 2 {
            return Ok(());
        }
    }
}

async fn answer(peer: &mut LivePeer, sequence: &mut u16, count: u16, text: &str) -> Result<(), String> {
    peer.send_text(&json!({"type":"response.audio_transcript.done","transcript":text}).to_string())
        .await
        .map_err(|_| "M3 Live: transcript send failed")?;
    for _ in 0..count {
        peer.send_audio(
            AudioFrame {
                sequence: *sequence,
                timestamp: u32::from(*sequence) * 960,
                payload: vec![0xf8, 0xff, 0xfe],
            },
            Instant::now().into_std(),
        )
        .await
        .map_err(|_| "M3 Live: reply send failed")?;
        *sequence += 1;
        if count == 10 {
            // Match the existing dialogue fixture's real media interval. Done
            // must not overtake RTP on the independent reliable data channel.
            let next = Instant::now() + Duration::from_millis(20);
            while Instant::now() < next {
                drive(peer, next).await?;
            }
        }
    }
    Ok(())
}

async fn drive(peer: &mut LivePeer, deadline: Instant) -> Result<(), String> {
    if Instant::now() >= deadline {
        return Err("M3 Live: phase deadline elapsed".into());
    }
    let next = peer
        .flush()
        .await
        .map_err(|_| "M3 Live: peer flush failed")?
        .min(deadline.into_std())
        .min((Instant::now() + Duration::from_millis(20)).into_std());
    tokio::select! {
        result = peer.readable() => {
            result.map_err(|_| "M3 Live: peer disconnected")?;
            peer.receive(Instant::now().into_std()).await.map_err(|_| "M3 Live: peer receive failed")?;
        }
        () = tokio::time::sleep_until(next.into()) => { peer.timeout(Instant::now().into_std()).await.map_err(|_| "M3 Live: peer timer failed")?; }
    }
    Ok(())
}

async fn request(socket: &mut TcpStream) -> Result<Value, String> {
    let mut bytes = Vec::new();
    let end = loop {
        if let Some(at) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break at + 4;
        }
        if bytes.len() >= 65536 {
            return Err("M3 Live: oversized signaling".into());
        }
        bytes.push(socket.read_u8().await.map_err(|_| "M3 Live: incomplete signaling")?);
    };
    let header = std::str::from_utf8(&bytes).map_err(|_| "M3 Live: invalid header")?;
    let length = header
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim())
        })
        .ok_or("M3 Live: missing length")?
        .parse::<usize>()
        .map_err(|_| "M3 Live: invalid length")?;
    if length > 65536 {
        return Err("M3 Live: oversized body".into());
    }
    bytes.resize(end + length, 0);
    socket
        .read_exact(&mut bytes[end..])
        .await
        .map_err(|_| "M3 Live: incomplete body")?;
    serde_json::from_slice(&bytes[end..]).map_err(|_| "M3 Live: invalid body".into())
}
