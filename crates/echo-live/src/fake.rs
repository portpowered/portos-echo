//! Reusable synthetic Live server on literal IPv4 loopback only.
//! No provider compatibility, decoded sound or real-service evidence is implied.
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

use crate::call::{CallError, drive, now};
use crate::{AudioFrame, LivePeer, fake_http};

pub use crate::fake_http::ObservedRequest;

/// Explicit fault injection, with no network retry or provider access.
#[derive(Clone)]
pub enum FakeResponse {
    Answer,
    Http {
        status: u16,
        body: String,
        location: Option<String>,
    },
    Disconnect,
    StallHttp,
    StallPeer,
    InvalidSdp(String),
}

/// Ordered synthetic event text; malformed events may be supplied for failure tests.
#[derive(Clone)]
pub struct FakeScript {
    pub events: Vec<String>,
    pub response: FakeResponse,
}

impl FakeScript {
    /// Load a synthetic script. This never accepts a captured real-provider trace.
    /// # Errors
    /// Returns Decode unless the document has synthetic=true and an event array.
    pub fn from_json(text: &str) -> Result<Self, CallError> {
        let document: serde_json::Value = serde_json::from_str(text).map_err(|_| CallError::Decode)?;
        if document["synthetic"] != true {
            return Err(CallError::Decode);
        }
        let events = document["events"]
            .as_array()
            .ok_or(CallError::Decode)?
            .iter()
            .map(serde_json::Value::to_string)
            .collect();
        Ok(Self {
            events,
            response: FakeResponse::Answer,
        })
    }
}

#[derive(Clone)]
pub struct ObservedCall {
    pub request: ObservedRequest,
    pub frames: Vec<AudioFrame>,
    pub commentary: Vec<String>,
    pub closed: bool,
}

#[derive(Clone, Default)]
pub struct FakeObserver(Arc<Mutex<Vec<ObservedCall>>>, Arc<tokio::sync::Notify>);

impl FakeObserver {
    /// Await recorded requests, without a timing sleep.
    /// # Errors
    /// Returns Signaling for a poisoned observer. Caller supplies its own deadline.
    pub async fn wait_for_calls(&self, count: usize) -> Result<(), CallError> {
        loop {
            let changed = self.1.notified();
            if self.calls()?.len() >= count {
                return Ok(());
            }
            changed.await;
        }
    }
    /// Snapshot after an explicit readiness/event signal, never after a fixed sleep.
    /// # Errors
    /// Returns Signaling if the observer was poisoned by a failed task.
    pub fn calls(&self) -> Result<Vec<ObservedCall>, CallError> {
        Ok(self.0.lock().map_err(|_| CallError::Signaling)?.clone())
    }

    fn record(&self, request: ObservedRequest) -> Result<usize, CallError> {
        let mut calls = self.0.lock().map_err(|_| CallError::Signaling)?;
        let index = calls.len();
        calls.push(ObservedCall {
            request,
            frames: Vec::new(),
            commentary: Vec::new(),
            closed: false,
        });
        self.1.notify_one();
        Ok(index)
    }

    fn update(&self, index: usize, update: impl FnOnce(&mut ObservedCall)) -> Result<(), CallError> {
        let mut calls = self.0.lock().map_err(|_| CallError::Signaling)?;
        update(calls.get_mut(index).ok_or(CallError::Signaling)?);
        Ok(())
    }
}

/// Drop aborts the listener and every per-call task, releasing their TCP/UDP sockets.
pub struct FakeLive {
    base: String,
    task: JoinHandle<()>,
    observer: FakeObserver,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl FakeLive {
    /// Bind an ephemeral loopback listener and start serving one script per call.
    /// # Errors
    /// Returns sanitized signaling errors for listener setup failures.
    pub async fn bind(script: FakeScript) -> Result<Self, CallError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| CallError::Signaling)?;
        let address = listener.local_addr().map_err(|_| CallError::Signaling)?;
        let mut base = reqwest::Url::parse("http://127.0.0.1").map_err(|_| CallError::Configuration)?;
        base.set_port(Some(address.port()))
            .map_err(|()| CallError::Configuration)?;
        let observer = FakeObserver::default();
        let records = observer.clone();
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break; };
                        let script = script.clone();
                        let records = records.clone();
                        tasks.spawn(async move { serve(stream, script, records).await });
                    }
                    _ = tasks.join_next(), if !tasks.is_empty() => (),
                }
            }
            tasks.shutdown().await;
            if let Ok(mut calls) = records.0.lock() {
                for call in calls.iter_mut() {
                    call.closed = true;
                }
            }
        });
        Ok(Self {
            base: base.to_string(),
            task,
            observer,
            stop: Some(stop),
        })
    }

    #[must_use]
    pub fn base_url(&self) -> String {
        self.base.clone()
    }

    #[must_use]
    pub fn observer(&self) -> FakeObserver {
        self.observer.clone()
    }

    /// Stop the listener, abort and join all call tasks, and release their sockets.
    pub async fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = (&mut self.task).await;
    }
}

impl Drop for FakeLive {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(mut stream: TcpStream, script: FakeScript, observer: FakeObserver) -> Result<(), CallError> {
    let request = fake_http::request(&mut stream).await?;
    let index = observer.record(request.clone())?;
    let mut peer = Box::pin(LivePeer::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), now())).await?;
    match script.response {
        FakeResponse::Answer => {
            let offer = request.body["sdp"].as_str().ok_or(CallError::Decode)?;
            let answer = peer.answer(offer).await?;
            fake_http::respond(&mut stream, 200, &answer, None).await?;
        }
        FakeResponse::Http { status, body, location } => {
            return fake_http::respond(&mut stream, status, &body, location.as_deref()).await;
        }
        FakeResponse::InvalidSdp(body) => return fake_http::respond(&mut stream, 200, &body, None).await,
        FakeResponse::Disconnect => return Ok(()),
        FakeResponse::StallHttp => return std::future::pending().await,
        FakeResponse::StallPeer => {
            let answer = peer
                .answer(request.body["sdp"].as_str().ok_or(CallError::Decode)?)
                .await?;
            fake_http::respond(&mut stream, 200, &answer, None).await?;
            return std::future::pending().await;
        }
    }
    let result = replay(&mut peer, &script.events, &observer, index).await;
    peer.close();
    observer.update(index, |call| call.closed = true)?;
    result
}

async fn replay(
    peer: &mut LivePeer,
    events: &[String],
    observer: &FakeObserver,
    index: usize,
) -> Result<(), CallError> {
    let deadline = now() + Duration::from_secs(20);
    while !peer.ready() {
        drive(peer, deadline).await?;
    }
    for event in events {
        peer.send_text(event).await?;
    }
    let mut count = 0_u64;
    loop {
        drive(peer, deadline).await?;
        while let Some(text) = peer.pop_text() {
            observer.update(index, |call| call.commentary.push(text))?;
            peer.send_text(r#"{"type":"fake.commentary.received"}"#).await?;
        }
        while let Some(frame) = peer.pop_audio() {
            count += 1;
            peer.send_audio(frame.clone(), now()).await?;
            observer.update(index, |call| call.frames.push(frame))?;
            peer.send_text(&format!(r#"{{"type":"fake.audio.frames","count":{count}}}"#))
                .await?;
        }
    }
}
