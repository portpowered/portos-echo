//! Copy complete frames unchanged, observing only after a successful write.
use echo_contracts::wyoming::{Decoder, Event, Header};
use std::net::SocketAddr;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{oneshot, watch},
    task::JoinHandle,
    time::Instant,
};

#[derive(Clone)]
pub(super) struct Receipt {
    pub direction: &'static str,
    pub kind: &'static str,
    pub elapsed_us: u128,
    pub pcm_bytes: usize,
}

pub(super) struct Relay {
    listener: Option<std::net::TcpListener>,
    task: Option<JoinHandle<Result<(), String>>>,
    stop: Option<oneshot::Sender<()>>,
    receipts: watch::Receiver<Vec<Receipt>>,
    observed: watch::Sender<Vec<Receipt>>,
}

impl Relay {
    pub fn new(listener: std::net::TcpListener) -> Self {
        let (observed, receipts) = watch::channel(Vec::new());
        Self {
            listener: Some(listener),
            task: None,
            stop: None,
            receipts,
            observed,
        }
    }

    pub fn start(&mut self, upstream: SocketAddr) -> Result<(), String> {
        let listener = TcpListener::from_std(self.listener.take().ok_or("device: wire already started")?)
            .map_err(|_| "device: wire runtime listener failed")?;
        let observed = self.observed.clone();
        let (stop, stopped) = oneshot::channel();
        self.stop = Some(stop);
        self.task = Some(tokio::spawn(async move {
            tokio::select! {
                biased;
                result = serve(listener, upstream, observed) => result,
                _ = stopped => Ok(()),
            }
        }));
        Ok(())
    }

    pub fn receipts(&self) -> Vec<Receipt> {
        self.receipts.borrow().clone()
    }

    #[cfg(test)]
    pub async fn wait_for_receipts(&mut self, count: usize) -> Result<(), String> {
        while self.receipts.borrow_and_update().len() < count {
            self.receipts
                .changed()
                .await
                .map_err(|_| "device: wire receipts closed")?;
        }
        Ok(())
    }

    pub async fn close(&mut self) -> Result<(), String> {
        self.listener.take();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        match self.task.take() {
            Some(task) => task.await.map_err(|_| "device: wire observer join failed")?,
            None => Ok(()),
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

async fn serve(
    listener: TcpListener,
    upstream: SocketAddr,
    observed: watch::Sender<Vec<Receipt>>,
) -> Result<(), String> {
    let (bridge, _) = listener.accept().await.map_err(|_| "device: wire accept failed")?;
    let host = TcpStream::connect(upstream)
        .await
        .map_err(|_| "device: wire upstream connect failed")?;
    let (mut bridge_in, mut bridge_out) = bridge.into_split();
    let (mut host_in, mut host_out) = host.into_split();
    let epoch = Instant::now();
    tokio::try_join!(
        forward(&mut host_in, &mut bridge_out, &observed, "capture", epoch),
        forward(&mut bridge_in, &mut host_out, &observed, "playback", epoch),
    )?;
    Ok(())
}

async fn forward(
    input: &mut (impl AsyncRead + Unpin),
    output: &mut (impl AsyncWrite + Unpin),
    observed: &watch::Sender<Vec<Receipt>>,
    direction: &'static str,
    epoch: Instant,
) -> Result<(), String> {
    while let Some((bytes, event)) = frame(input).await? {
        output
            .write_all(&bytes)
            .await
            .map_err(|_| "device: wire forward write failed")?;
        let elapsed_us = epoch.elapsed().as_micros();
        let kind = match event.header.r#type.as_str() {
            "run-pipeline" => "run-pipeline",
            "audio-start" => "audio-start",
            "audio-chunk" => "audio-chunk",
            "audio-stop" => "audio-stop",
            "played" => "played",
            "hassmic-continue" => "hassmic-continue",
            "hassmic-flush" => "hassmic-flush",
            _ => continue,
        };
        if observed.borrow().len() >= 16384 {
            return Err("device: wire receipt limit exceeded".into());
        }
        observed.send_modify(|receipts| {
            receipts.push(Receipt {
                direction,
                kind,
                elapsed_us,
                pcm_bytes: if kind == "audio-chunk" { event.payload.len() } else { 0 },
            })
        });
    }
    output
        .shutdown()
        .await
        .map_err(|_| "device: wire half-close failed".into())
}

async fn frame(input: &mut (impl AsyncRead + Unpin)) -> Result<Option<(Vec<u8>, Event)>, String> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0_u8];
        match input.read(&mut byte).await {
            Ok(0) if bytes.is_empty() => return Ok(None),
            Ok(1) => bytes.push(byte[0]),
            _ => return Err("device: malformed wire header".into()),
        }
        if bytes.len() > 4096 {
            return Err("device: oversized wire header".into());
        }
        if byte[0] == b'\n' {
            break;
        }
    }
    let header: Header = serde_json::from_slice(&bytes).map_err(|_| "device: malformed wire header")?;
    let size = header
        .data_length
        .checked_add(header.payload_length)
        .filter(|size| *size <= 65536)
        .ok_or("device: oversized wire frame")?;
    let offset = bytes.len();
    bytes.resize(offset + size, 0);
    input
        .read_exact(&mut bytes[offset..])
        .await
        .map_err(|_| "device: incomplete wire frame")?;
    let mut decoder = Decoder::new();
    let event = decoder
        .feed(&bytes)
        .map_err(|_| "device: malformed wire frame")?
        .pop()
        .ok_or("device: missing wire frame")?;
    decoder.finish().map_err(|_| "device: incomplete wire frame")?;
    Ok(Some((bytes, event)))
}
