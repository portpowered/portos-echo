use std::net::SocketAddr;
use std::time::{Duration, Instant};

use rtc::data_channel::RTCDataChannelId;
use rtc::peer_connection::RTCPeerConnection;
use rtc::peer_connection::event::{RTCDataChannelEvent, RTCPeerConnectionEvent};
use rtc::peer_connection::message::RTCMessage;
use rtc::peer_connection::state::RTCIceConnectionState;
use rtc::sansio::Protocol;
use rtc::shared::{TaggedBytesMut, TransportContext, TransportProtocol};
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
    rtc: RTCPeerConnection,
    now: Instant,
    socket: Option<UdpSocket>,
    address: SocketAddr,
    negotiation: Negotiation,
    channel: Option<RTCDataChannelId>,
    connected: bool,
    audio: Queue<AudioFrame>,
    text: Queue<String>,
}

impl LivePeer {
    pub(crate) async fn from_socket(socket: UdpSocket, now: Instant) -> Result<Self, Error> {
        let address = socket.local_addr().map_err(|_| Error::Io)?;
        // rtc's host nomination uses elapsed > 0, even with zero minimum wait.
        // Seed signaling one microsecond before the initial tick so a datagram
        // at the caller's initial tick can nominate. All later input, media and
        // timeout times stay exactly caller-owned; no clock or task is started.
        // A nanosecond rounds to zero at Windows Instant's counter resolution.
        let now = now.checked_sub(Duration::from_micros(1)).ok_or(Error::Protocol)?;
        let rtc = crate::negotiation::connection(address, now)?;
        let mut peer = Self {
            rtc,
            now,
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
        let offer = self.negotiation.offer(&mut self.rtc, self.now)?;
        self.flush().await?;
        Ok(offer)
    }
    /// Accept an offer. Errors omit remote SDP.
    /// # Errors
    /// Returns closed, state or invalid-SDP errors.
    pub async fn answer(&mut self, offer: &str) -> Result<String, Error> {
        self.open()?;
        let answer = self.negotiation.answer(&mut self.rtc, self.now, offer)?;
        self.flush().await?;
        Ok(answer)
    }
    /// Apply an outstanding offer's answer.
    /// # Errors
    /// Returns closed, state or invalid-SDP errors.
    pub async fn apply_answer(&mut self, answer: &str) -> Result<(), Error> {
        self.open()?;
        self.negotiation.apply_answer(&mut self.rtc, self.now, answer)?;
        self.flush().await?;
        Ok(())
    }

    /// Send one encoded frame. The caller provides sequence/timestamps and pacing.
    /// # Errors
    /// Returns closed/readiness/payload/protocol/I/O errors.
    pub async fn send_audio(&mut self, frame: AudioFrame, now: Instant) -> Result<(), Error> {
        self.check_ready()?;
        audio_payload(frame.payload.len())?;
        self.now = now;
        self.negotiation.send_audio(&mut self.rtc, now, frame)?;
        self.flush().await?;
        Ok(())
    }

    /// Send opaque UTF-8 text; `Full` means it was not accepted, and may be retried.
    /// # Errors
    /// Returns closed/readiness/pressure/protocol/I/O errors.
    pub async fn send_text(&mut self, text: &str) -> Result<(), Error> {
        self.check_ready()?;
        text_payload(text.len())?;
        let mut channel = self
            .rtc
            .data_channel(self.channel.ok_or(Error::NotReady)?)
            .ok_or(Error::NotReady)?;
        if channel.outstanding_bytes().saturating_add(text.len()) > 128 * 1024 {
            return Err(Error::Full);
        }
        channel.send_text(self.now, text).map_err(|_| Error::Protocol)?;
        self.flush().await?;
        Ok(())
    }

    /// Drain all WebRTC outputs through Timeout before the next mutation.
    /// # Errors
    /// Returns closed/protocol/I/O/receive-pressure errors.
    pub async fn flush(&mut self) -> Result<Instant, Error> {
        self.open()?;
        loop {
            // Event handling can create a DTLS endpoint whose initial flight
            // is collected by handle_timeout, even before its next deadline.
            // Service it at the last supplied instant, without advancing time.
            self.rtc.handle_timeout(self.now).map_err(|_| Error::Protocol)?;
            let mut progressed = false;
            while let Some(packet) = self.rtc.poll_write() {
                progressed = true;
                self.socket
                    .as_ref()
                    .ok_or(Error::Closed)?
                    .send_to(&packet.message, packet.transport.peer_addr)
                    .await
                    .map_err(|_| Error::Io)?;
            }
            while let Some(event) = self.rtc.poll_event() {
                progressed = true;
                self.event(&event);
            }
            while let Some(packet) = self.rtc.poll_read() {
                progressed = true;
                self.message(packet.message)?;
            }
            // Draining events can start DTLS/SCTP and enqueue more writes.
            // Finish that work before waiting for another datagram or tick.
            if !progressed {
                break;
            }
        }
        Ok(self.rtc.poll_timeout().unwrap_or(self.now + Duration::from_hours(24)))
    }

    /// Wait for socket readiness. Cancelling this future preserves queued data.
    /// # Errors
    /// Returns `Closed` or `Io`.
    pub fn readable(&self) -> impl Future<Output = Result<(), Error>> + Send + use<'_> {
        let socket = self.socket.as_ref();
        async move { socket.ok_or(Error::Closed)?.readable().await.map_err(|_| Error::Io) }
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
        datagram(&buf[..len])?;
        self.now = now;
        self.rtc
            .handle_read(TaggedBytesMut {
                now,
                transport: TransportContext {
                    local_addr: self.address,
                    peer_addr: source,
                    transport_protocol: TransportProtocol::UDP,
                    ecn: None,
                },
                message: (&buf[..len]).into(),
            })
            .map_err(|_| Error::Protocol)?;
        self.flush().await?;
        Ok(true)
    }

    /// Advance the injected protocol clock at a scheduled timeout.
    /// # Errors
    /// Returns closed/protocol/I/O/pressure errors.
    pub async fn timeout(&mut self, now: Instant) -> Result<(), Error> {
        self.open()?;
        self.now = now;
        self.rtc.handle_timeout(now).map_err(|_| Error::Protocol)?;
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
        let _ = self.rtc.close();
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

    fn event(&mut self, event: &RTCPeerConnectionEvent) {
        match event {
            RTCPeerConnectionEvent::OnIceConnectionStateChangeEvent(state) => {
                self.connected = matches!(
                    state,
                    RTCIceConnectionState::Connected | RTCIceConnectionState::Completed
                );
            }
            RTCPeerConnectionEvent::OnDataChannel(RTCDataChannelEvent::OnOpen(id)) => {
                if self.rtc.data_channel(*id).is_some_and(|c| c.label() == "oai-events") {
                    self.channel = Some(*id);
                }
            }
            RTCPeerConnectionEvent::OnDataChannel(
                RTCDataChannelEvent::OnClose(id)
                | RTCDataChannelEvent::OnClosing(id)
                | RTCDataChannelEvent::OnError(id),
            ) if self.channel == Some(*id) => self.channel = None,
            _ => {}
        }
    }

    fn message(&mut self, message: RTCMessage) -> Result<(), Error> {
        match message {
            RTCMessage::DataChannelMessage(id, data) if self.channel == Some(id) => {
                if !data.is_string {
                    return Err(Error::Payload);
                }
                let text = String::from_utf8(data.data.to_vec()).map_err(|_| Error::Payload)?;
                self.text.push(text, false)?;
            }
            RTCMessage::RtpPacket(_, packet) => self.audio.push(
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

fn audio_payload(len: usize) -> Result<(), Error> {
    if (1..=1200).contains(&len) {
        Ok(())
    } else {
        Err(Error::Payload)
    }
}

fn text_payload(len: usize) -> Result<(), Error> {
    if len <= 16_384 { Ok(()) } else { Err(Error::Full) }
}

// Preserve the public malformed-datagram error before rtc's tolerant demuxer.
// RFC 7983 assigns the first-byte ranges for STUN, DTLS and RTP/RTCP.
fn datagram(bytes: &[u8]) -> Result<(), Error> {
    match bytes.first() {
        Some(0..=1) if bytes.len() >= 20 => rtc::stun::message::Message::default()
            .unmarshal_binary(bytes)
            .map_err(|_| Error::Protocol),
        Some(20..=63) => Ok(()),
        Some(128..=191) if bytes.len() > 2 => Ok(()),
        _ => Err(Error::Protocol),
    }
}

#[cfg(test)]
#[path = "peer/peer_tests.rs"]
mod tests;
