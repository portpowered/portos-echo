//! Encoded Opus and opaque event text over one WebRTC connection.
//! The caller owns signaling and protocol time. No provider requests or credentials.
mod negotiation;
mod peer;
mod queue;

pub use peer::{AudioFrame, LivePeer};

impl LivePeer {
    /// Bind one explicit host candidate; no interface discovery, STUN or TURN.
    ///
    /// # Errors
    /// Returns `Io` if binding fails or `Protocol` if candidate setup fails.
    pub async fn bind(address: std::net::SocketAddr, now: std::time::Instant) -> Result<Self, Error> {
        let socket = tokio::net::UdpSocket::bind(address).await.map_err(|_| Error::Io)?;
        Box::pin(Self::from_socket(socket, now)).await
    }
}

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
