//! Encoded Opus and opaque event text over one WebRTC connection.
//! The caller owns signaling and protocol time. No provider requests or credentials.
mod negotiation;
mod peer;
mod queue;

pub use peer::{AudioFrame, LivePeer};

/// Errors deliberately omit remote SDP and payloads.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("transport is closed")]
    Closed,
    #[error("operation is invalid in this negotiation state")]
    State,
    #[error("invalid SDP")]
    InvalidSdp,
    #[error("transport is not ready")]
    NotReady,
    #[error("text queue is full")]
    Full,
    #[error("unsupported transport payload")]
    Payload,
    #[error("WebRTC protocol error")]
    Protocol,
    #[error("UDP I/O error")]
    Io,
    #[error("deadline reached")]
    Deadline,
}
