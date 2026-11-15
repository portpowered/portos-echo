use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use str0m::channel::ChannelId;
use str0m::format::Codec;
use str0m::net::{Protocol, Receive};
use str0m::rtp::RtpWrite;
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tokio::net::UdpSocket;

use crate::{Error, negotiation::Negotiation, queue::Queue};

/// One encoded 20-ms Opus packet. Timestamps use the 48-kHz RTP clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioFrame {
    pub sequence: u16,
    pub timestamp: u32,
    pub payload: Vec<u8>,
}

/// Single-owner connection with no background tasks. Call `flush` after signaling,
/// `receive` when readable, and `timeout` at the returned protocol deadline.
/// All times must be monotonic and use the same caller-owned clock.
/// Drop/close releases the UDP socket; cancellation of a receive never owns a task.
pub struct LivePeer {
    rtc: Rtc,
    socket: Option<UdpSocket>,
    address: SocketAddr,
    negotiation: Negotiation,
    channel: Option<ChannelId>,
    connected: bool,
    audio: Queue<AudioFrame>,
    text: Queue<String>,
}

impl LivePeer {
    /// Bind one explicit host candidate; no interface discovery, STUN or TURN.
    ///
    /// # Errors
    /// Returns `Io` if binding fails or `Protocol` if candidate setup fails.
    pub async fn bind(address: SocketAddr, now: Instant) -> Result<Self, Error> {
        let socket = UdpSocket::bind(address).await.map_err(|_| Error::Io)?;
        let address = socket.local_addr().map_err(|_| Error::Io)?;
        let mut rtc = Rtc::builder()
            .set_crypto_provider(Arc::new(str0m::crypto::from_feature_flags()))
            .set_rtp_mode(true)
            .clear_codecs()
            .enable_opus(true, false)
            .set_sctp_max_message_size(16_384)
            .build(now);
        rtc.add_local_candidate(Candidate::host(address, "udp").map_err(|_| Error::Protocol)?)
            .ok_or(Error::Protocol)?;
        let mut peer = Self {
            rtc,
            socket: Some(socket),
            address,
            negotiation: Negotiation::default(),
            channel: None,
            connected: false,
            audio: Queue::new(128),
            text: Queue::new(16),
        };
        peer.flush().await?;
        Ok(peer)
    }

    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    #[must_use]
    pub fn ready(&self) -> bool {
        self.socket.is_some() && self.connected && self.channel.is_some()
    }

    /// Create one sendrecv Opus track and one reliable ordered `oai-events` channel.
    /// # Errors
    /// Returns `Closed` or `State` when negotiation cannot start.
    pub async fn offer(&mut self) -> Result<String, Error> {
        self.open()?;
        let offer = self.negotiation.offer(&mut self.rtc)?;
        self.flush().await?;
        Ok(offer)
    }
    /// Accept an offer. Errors omit remote SDP.
    /// # Errors
    /// Returns closed, state or invalid-SDP errors.
    pub async fn answer(&mut self, offer: &str) -> Result<String, Error> {
        self.open()?;
        let answer = self.negotiation.answer(&mut self.rtc, offer)?;
        self.flush().await?;
        Ok(answer)
    }
    /// Apply an outstanding offer's answer.
    /// # Errors
    /// Returns closed, state or invalid-SDP errors.
    pub async fn apply_answer(&mut self, answer: &str) -> Result<(), Error> {
        self.open()?;
        self.negotiation.apply_answer(&mut self.rtc, answer)?;
        self.flush().await?;
        Ok(())
    }

    /// Send one encoded frame. The caller provides sequence/timestamps and pacing.
    /// # Errors
    /// Returns closed/readiness/payload/protocol/I/O errors.
    pub async fn send_audio(&mut self, frame: AudioFrame, now: Instant) -> Result<(), Error> {
        self.check_ready()?;
        if frame.payload.is_empty() || frame.payload.len() > 1200 {
            return Err(Error::Payload);
        }
        let mid = self.negotiation.mid.ok_or(Error::NotReady)?;
        let pt = self
            .rtc
            .codec_config()
            .params()
            .iter()
            .find(|p| p.spec().codec == Codec::Opus)
            .map(str0m::format::PayloadParams::pt)
            .ok_or(Error::NotReady)?;
        self.rtc
            .direct_api()
            .stream_tx_by_mid(mid, None)
            .ok_or(Error::NotReady)?
            .write_rtp(RtpWrite::new(
                pt,
                u64::from(frame.sequence).into(),
                frame.timestamp,
                now,
                frame.payload,
            ));
        self.flush().await?;
        Ok(())
    }

    /// Send opaque UTF-8 text; `Full` means it was not accepted, and may be retried.
    /// # Errors
    /// Returns closed/readiness/pressure/protocol/I/O errors.
    pub async fn send_text(&mut self, text: &str) -> Result<(), Error> {
        self.check_ready()?;
        if text.len() > 16_384 {
            return Err(Error::Full);
        }
        let mut channel = self
            .rtc
            .channel(self.channel.ok_or(Error::NotReady)?)
            .ok_or(Error::NotReady)?;
        if !channel.write(false, text.as_bytes()).map_err(|_| Error::Protocol)? {
            return Err(Error::Full);
        }
        self.flush().await?;
        Ok(())
    }

    /// Drain all WebRTC outputs through Timeout before the next mutation.
    /// # Errors
    /// Returns closed/protocol/I/O/receive-pressure errors.
    pub async fn flush(&mut self) -> Result<Instant, Error> {
        self.open()?;
        loop {
            match self.rtc.poll_output().map_err(|_| Error::Protocol)? {
                Output::Timeout(deadline) => return Ok(deadline),
                Output::Transmit(packet) => {
                    self.socket
                        .as_ref()
                        .ok_or(Error::Closed)?
                        .send_to(&packet.contents, packet.destination)
                        .await
                        .map_err(|_| Error::Io)?;
                }
                Output::Event(event) => self.event(event)?,
            }
        }
    }

    /// Wait for socket readiness. Cancelling this future preserves queued data.
    /// # Errors
    /// Returns `Closed` or `Io`.
    pub async fn readable(&self) -> Result<(), Error> {
        self.socket
            .as_ref()
            .ok_or(Error::Closed)?
            .readable()
            .await
            .map_err(|_| Error::Io)
    }

    /// Consume one datagram after readiness; false denotes spurious readiness.
    /// # Errors
    /// Returns closed, I/O, malformed datagram, protocol or queue pressure errors.
    pub async fn receive(&mut self, now: Instant) -> Result<bool, Error> {
        let mut buf = [0; 2048];
        let result = self.socket.as_ref().ok_or(Error::Closed)?.try_recv_from(&mut buf);
        let (len, source) = match result {
            Ok(value) => value,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
            Err(_) => return Err(Error::Io),
        };
        let received = Receive::new(Protocol::Udp, source, self.address, &buf[..len]).map_err(|_| Error::Protocol)?;
        self.rtc
            .handle_input(Input::Receive(now, received))
            .map_err(|_| Error::Protocol)?;
        self.flush().await?;
        Ok(true)
    }

    /// Advance the injected protocol clock at a scheduled timeout.
    /// # Errors
    /// Returns closed/protocol/I/O/pressure errors.
    pub async fn timeout(&mut self, now: Instant) -> Result<(), Error> {
        self.open()?;
        self.rtc
            .handle_input(Input::Timeout(now))
            .map_err(|_| Error::Protocol)?;
        self.flush().await?;
        Ok(())
    }

    #[must_use]
    pub fn pop_audio(&mut self) -> Option<AudioFrame> {
        self.audio.pop()
    }

    #[must_use]
    pub fn pop_text(&mut self) -> Option<String> {
        self.text.pop()
    }

    #[must_use]
    pub fn dropped_audio(&self) -> u64 {
        self.audio.drops
    }

    /// Idempotent; keeps already received values available for draining.
    pub fn close(&mut self) {
        self.rtc.disconnect();
        self.socket.take();
        self.negotiation = Negotiation::default();
        self.connected = false;
        self.channel = None;
        self.audio.close();
        self.text.close();
    }

    fn open(&self) -> Result<(), Error> {
        if self.socket.is_none() {
            Err(Error::Closed)
        } else {
            Ok(())
        }
    }

    fn check_ready(&self) -> Result<(), Error> {
        self.open()?;
        if self.ready() { Ok(()) } else { Err(Error::NotReady) }
    }

    fn event(&mut self, event: Event) -> Result<(), Error> {
        match event {
            Event::IceConnectionStateChange(state) => {
                self.connected = matches!(state, IceConnectionState::Connected | IceConnectionState::Completed);
            }
            Event::MediaAdded(media) => self.negotiation.mid = Some(media.mid),
            Event::ChannelOpen(id, label) if label == "oai-events" => self.channel = Some(id),
            Event::ChannelClose(id) if self.channel == Some(id) => self.channel = None,
            Event::ChannelData(data) if self.channel == Some(data.id) => {
                if data.binary {
                    return Err(Error::Payload);
                }
                let text = String::from_utf8(data.data).map_err(|_| Error::Payload)?;
                self.text.push(text, false)?;
            }
            Event::RtpPacket(packet) => self.audio.push(
                AudioFrame {
                    sequence: packet.header.sequence_number,
                    timestamp: packet.header.timestamp,
                    payload: packet.payload.to_vec(),
                },
                true,
            )?,
            _ => {}
        }
        Ok(())
    }
}
