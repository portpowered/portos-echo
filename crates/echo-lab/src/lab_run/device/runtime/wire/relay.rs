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

use super::ledger::{Ledger, Receipt};

pub(super) struct Relay {
    listener: Option<std::net::TcpListener>,
    task: Option<JoinHandle<Result<(), String>>>,
    stop: Option<oneshot::Sender<()>>,
    receipts: watch::Receiver<Ledger>,
    observed: watch::Sender<Ledger>,
}

impl Relay {
    pub fn new(listener: std::net::TcpListener) -> Self {
        let (observed, receipts) = watch::channel(Ledger::default());
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
            let result = tokio::select! {
                biased;
                result = serve(listener, upstream, observed.clone()) => result,
                _ = stopped => Ok(()),
            };
            if let Err(error) = &result {
                observed.send_modify(|ledger| ledger.failure = Some(error.clone()));
            }
            observed.send_modify(|ledger| ledger.closed = true);
            result
        }));
        Ok(())
    }

    pub fn receipts(&self) -> Vec<Receipt> {
        self.receipts.borrow().receipts.clone()
    }

    pub fn session(&self) -> serde_json::Value {
        self.receipts.borrow().session.observation()
    }

    pub async fn wait_reply(&mut self, run: usize, replies: usize) -> Result<(), String> {
        loop {
            {
                let ledger = self.receipts.borrow_and_update();
                if let Some(error) = &ledger.failure {
                    return Err(error.clone());
                }
                if ledger.closed {
                    return Err("device: unmet prerequisite: Played before wire close".into());
                }
                if ledger.session.checkpoint(run, replies)? {
                    return Ok(());
                }
            }
            self.receipts
                .changed()
                .await
                .map_err(|_| "device: wire closed before reply barrier")?;
        }
    }

    #[cfg(test)]
    pub async fn wait_for_receipts(&mut self, count: usize) -> Result<(), String> {
        while self.receipts.borrow_and_update().receipts.len() < count {
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

async fn serve(listener: TcpListener, upstream: SocketAddr, observed: watch::Sender<Ledger>) -> Result<(), String> {
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
    observed: &watch::Sender<Ledger>,
    direction: &'static str,
    epoch: Instant,
) -> Result<(), String> {
    while let Some((bytes, event)) = frame(input).await? {
        output
            .write_all(&bytes)
            .await
            .map_err(|_| "device: wire forward write failed")?;
        let mut result = Ok(());
        // Sample under the same lock that orders both directions. Sampling
        // before the lock could publish backwards timestamps under contention.
        observed.send_modify(|ledger| {
            result = ledger.observe(direction, &event, epoch.elapsed().as_micros());
        });
        result?;
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
