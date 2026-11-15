//! Provisional provider adapter. Actual provider compatibility belongs to M2.
use std::collections::BTreeMap;
use std::time::Instant;

use echo_contracts::ids::{DelegationId, SessionId};
use echo_contracts::live::LiveEvent;
use echo_contracts::secret::Secret;

use crate::{AudioFrame, Error, LivePeer, call_http};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveAuthMode {
    Chatgpt,
    ApiKey,
}

/// Implementations own sign-in and storage; this crate never refreshes credentials.
pub trait CredentialProvider {
    fn credential(&self) -> impl Future<Output = Result<LiveCredential, CallError>> + Send;
}

/// Deliberately implements neither Debug nor Display.
pub enum LiveCredential {
    Chatgpt { token: Secret<String>, account_id: String },
    ApiKey { key: Secret<String> },
}

pub struct SessionOptions {
    pub model: String,
    pub voice: String,
    pub instructions: String,
    pub session_id: SessionId,
    pub thread_id: String,
}

/// Safe categories only: no provider body, credential, URL or SDP diagnostics.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    #[error("invalid Live configuration")]
    Configuration,
    #[error("Live authorization requires relinking")]
    Authorization,
    #[error("Live provider unavailable")]
    Unavailable,
    #[error("Live signaling failed")]
    Signaling,
    #[error("Live deadline reached")]
    Deadline,
    #[error("invalid Live event")]
    Decode,
    #[error("Live transport: {0}")]
    Transport(#[from] Error),
}

pub struct LiveCallFactory<P> {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    mode: LiveAuthMode,
    provider: P,
}

impl<P: CredentialProvider> LiveCallFactory<P> {
    /// A base origin must use HTTPS, or HTTP on literal 127.0.0.1 for local fakes.
    /// # Errors
    /// Returns sanitized configuration errors for an invalid origin or HTTP setup.
    pub fn new(base: &str, mode: LiveAuthMode, provider: P) -> Result<Self, CallError> {
        let endpoint = call_http::endpoint(base, mode)?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .build()
            .map_err(|_| CallError::Configuration)?;
        Ok(Self {
            client,
            endpoint,
            mode,
            provider,
        })
    }

    /// Consume a fresh peer, create exactly one offer and POST, then wait for readiness.
    /// Dropping/cancelling this future releases its peer. No retry task is spawned.
    /// # Errors
    /// Returns safe validation, authorization, signaling, deadline or transport errors.
    pub fn create(
        &self,
        peer: LivePeer,
        options: SessionOptions,
        deadline: Instant,
    ) -> impl Future<Output = Result<LiveCall, CallError>> + '_ {
        let peer = Box::new(peer);
        async move {
            if now() >= deadline {
                return Err(CallError::Deadline);
            }
            tokio::time::timeout_at(deadline.into(), self.create_inner(peer, options, deadline))
                .await
                .map_err(|_| CallError::Deadline)?
        }
    }

    async fn create_inner(
        &self,
        mut peer: Box<LivePeer>,
        options: SessionOptions,
        deadline: Instant,
    ) -> Result<LiveCall, CallError> {
        call_http::validate_options(self.mode, &options)?;
        let credential = self.provider.credential().await?;
        let headers = call_http::headers(self.mode, &credential, &options)?;
        let offer = peer.offer().await?;
        let body = call_http::offer_body(&offer, &options);
        let response = self
            .client
            .post(self.endpoint.clone())
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|_| CallError::Signaling)?;
        let answer = call_http::answer(response).await?;
        peer.apply_answer(&answer).await?;
        while !peer.ready() {
            drive(&mut peer, deadline).await?;
        }
        Ok(LiveCall { peer, closed: false })
    }
}

/// Single-owner configured call. Keep driving receive to service WebRTC timers.
/// Sends are never replayed; cancel an uncertain send by closing the journey.
pub struct LiveCall {
    peer: Box<LivePeer>,
    closed: bool,
}

impl LiveCall {
    /// Receive one typed event, preserving unknown payloads and known extra fields.
    /// # Errors
    /// Closed, deadline, transport or sanitized decoding errors; none fabricate events.
    pub async fn receive_event(&mut self, deadline: Instant) -> Result<LiveEvent, CallError> {
        self.open()?;
        loop {
            if let Some(text) = self.peer.pop_text() {
                return decode(&text);
            }
            drive(&mut self.peer, deadline).await?;
        }
    }

    /// Send exactly the supplied delegation ID and text; never selects a delegation target.
    /// # Errors
    /// Returns closed, serialization or transport pressure errors without replaying sends.
    pub async fn append_commentary(&mut self, id: DelegationId, text: String) -> Result<(), CallError> {
        self.open()?;
        let text = commentary(id, text)?;
        self.peer.send_text(&text).await?;
        Ok(())
    }

    /// Forward one encoded frame without resampling or retries.
    /// # Errors
    /// Returns closed, readiness, payload or transport errors.
    pub async fn send_audio(&mut self, frame: AudioFrame, now: Instant) -> Result<(), CallError> {
        self.open()?;
        self.peer.send_audio(frame, now).await?;
        Ok(())
    }

    /// Wait for encoded audio while retaining event text in the bounded FIFO.
    /// # Errors
    /// Returns closed, deadline or transport pressure errors. Close on overflow.
    pub async fn receive_audio(&mut self, deadline: Instant) -> Result<AudioFrame, CallError> {
        self.open()?;
        loop {
            if let Some(frame) = self.peer.pop_audio() {
                return Ok(frame);
            }
            drive(&mut self.peer, deadline).await?;
        }
    }

    #[must_use]
    pub fn pop_audio(&mut self) -> Option<AudioFrame> {
        self.peer.pop_audio()
    }

    #[must_use]
    pub fn dropped_audio(&self) -> u64 {
        self.peer.dropped_audio()
    }

    /// Releases local resources only; no invented remote DELETE endpoint.
    pub fn close(&mut self) {
        self.closed = true;
        self.peer.close();
    }

    fn open(&self) -> Result<(), CallError> {
        if self.closed { Err(Error::Closed.into()) } else { Ok(()) }
    }
}

pub(crate) fn decode(text: &str) -> Result<LiveEvent, CallError> {
    serde_json::from_str(text).map_err(|_| CallError::Decode)
}

pub(crate) fn commentary(id: DelegationId, text: String) -> Result<String, CallError> {
    let event = LiveEvent::CommentaryAppend {
        delegation_id: id,
        text,
        extra: BTreeMap::new(),
    };
    let text = serde_json::to_string(&event).map_err(|_| CallError::Decode)?;
    if text.len() > 16_384 {
        return Err(Error::Full.into());
    }
    Ok(text)
}

pub(crate) fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

pub(crate) async fn drive(peer: &mut LivePeer, deadline: Instant) -> Result<(), CallError> {
    if now() >= deadline {
        return Err(CallError::Deadline);
    }
    let next = peer.flush().await?.min(deadline);
    let readable = tokio::select! {
        result = peer.readable() => { result?; true },
        () = tokio::time::sleep_until(next.into()) => false,
    };
    if readable {
        peer.receive(now()).await?;
    } else {
        peer.timeout(now()).await?;
    }
    Ok(())
}
