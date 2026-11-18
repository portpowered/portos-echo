// Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0
// Adapted from Codex voice-host at a956835; Modified by Port OS.
//! Frame-based WebRTC audio. Signaling and authentication belong to the caller.
use std::{future::Future, net::IpAddr, num::NonZeroUsize, time::Duration};
use tokio::sync::watch;

mod media_audio;
mod media_ingress;
mod media_peer;
mod media_queue;
mod media_runtime;
mod media_signal;
mod media_state;
mod media_track;
pub use media_peer::Peer;

#[cfg(test)]
#[path = "media_tests/media_tests.rs"]
mod media_tests;

/// PCM16 mono, 16 kHz, twenty milliseconds.
pub type PcmFrame = [i16; 320];

/// Local interface addresses, optional STUN, and bounded resource budgets.
#[derive(Clone, Debug)]
pub struct PeerConfig {
    pub interface_allowlist: Vec<IpAddr>,
    pub allow_loopback: bool,
    pub stun_servers: Vec<String>,
    pub queue_capacity: NonZeroUsize,
    pub negotiation_timeout: Duration,
}

impl Default for PeerConfig {
    fn default() -> Self {
        Self {
            interface_allowlist: Vec::new(),
            allow_loopback: false,
            stun_servers: Vec::new(),
            queue_capacity: NonZeroUsize::new(10).unwrap_or(NonZeroUsize::MIN),
            negotiation_timeout: Duration::from_secs(15),
        }
    }
}

/// Latest connection state; Closed is terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerState {
    New,
    Connecting,
    Connected,
    Failed,
    Closed,
}

/// Payload-free observations of actual transport setup transitions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupStage {
    IceConnected,
    DataChannelOpen,
}

/// Called synchronously once per stage. Must not block or reenter the peer.
pub type SetupObserver = std::sync::Arc<dyn Fn(SetupStage) -> Result<(), Error> + Send + Sync>;

/// Fixed error categories never contain native diagnostics, SDP, or PCM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidConfig,
    InvalidSdp,
    State,
    NotConnected,
    Deadline,
    Transport,
    Audio,
    Closed,
}

/// Cumulative oldest-frame evictions, independently for each direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DropCounts {
    pub send: u64,
    pub recv: u64,
}

/// Caller boundary. Configuration selects assigned IP addresses, never remote peers.
pub trait MediaPeer: Sized {
    fn create(config: PeerConfig) -> impl Future<Output = Result<Self, Error>> + Send;
    fn offer(&self) -> impl Future<Output = Result<String, Error>> + Send;
    fn apply_answer(&self, sdp: &str) -> impl Future<Output = Result<(), Error>> + Send;
    fn states(&self) -> watch::Receiver<PeerState>;
    /// Install once, before offer. Unsupported peers reject observation.
    /// # Errors
    /// Returns State for unsupported, repeated or late installation.
    fn observe_setup(&self, observer: SetupObserver) -> Result<(), Error> {
        drop(observer);
        Err(Error::State)
    }
    /// # Errors
    /// Returns `NotConnected` before readiness, the fixed failure category, or Closed.
    fn send(&self, frame: PcmFrame) -> Result<(), Error>;
    fn recv(&mut self) -> impl Future<Output = Result<Option<PcmFrame>, Error>> + Send;
    fn dropped_frames(&self) -> DropCounts;
    fn close(&mut self) -> impl Future<Output = Result<(), Error>> + Send;
}
