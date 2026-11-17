//! Joint signaling/media owner; readiness requires ICE, DTLS and the data channel.
use super::{Call, Event, Options};
use crate::Error;
use echo_codex::{
    DynamicTool,
    codex_io::RpcIo,
    codex_protocol::{RequestId, ToolResult},
};
use echo_media::{DropCounts, MediaPeer, PcmFrame, Peer, PeerState};
use std::collections::VecDeque;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::{Instant, timeout_at},
};

/// Media and signaling remain independently observable. `ReplyDone` does not drain PCM.
pub enum MediaEvent {
    Control(Event),
    Audio(Box<PcmFrame>),
}

/// One call owns its media peer and exclusively borrows initialized app-server IO.
/// Always await close, then supervisor shutdown, on host failure/cancellation.
pub struct MediaCall<'a, R: AsyncRead + Unpin, W: AsyncWrite + Unpin, P: MediaPeer = Peer> {
    call: Call<'a, R, W>,
    peer: P,
    early: VecDeque<Event>,
    started: bool,
    active: bool,
    closed: bool,
}

impl<'a, R: AsyncRead + Unpin, W: AsyncWrite + Unpin, P: MediaPeer> MediaCall<'a, R, W, P> {
    pub fn new(io: &'a mut RpcIo<R, W>, peer: P) -> Self {
        Self {
            call: Call::new(io),
            peer,
            early: VecDeque::new(),
            started: false,
            active: false,
            closed: false,
        }
    }

    /// Offer, matching answer and connected media share one absolute deadline.
    /// # Errors
    /// No connected result on signaling-only success; failures close both owners.
    pub async fn start(
        &mut self,
        options: &Options<'_>,
        tools: Vec<DynamicTool>,
        deadline: Instant,
    ) -> Result<(), Error> {
        if self.started || self.closed {
            return Err(Error::Input);
        }
        self.started = true;
        let result = timeout_at(deadline, self.negotiate(options, tools, deadline))
            .await
            .unwrap_or(Err(Error::Deadline));
        if result.is_err() {
            self.call.fail();
            let _ = self.peer.close().await;
            self.closed = true;
        } else {
            self.active = true;
        }
        result
    }

    async fn negotiate(
        &mut self,
        options: &Options<'_>,
        tools: Vec<DynamicTool>,
        deadline: Instant,
    ) -> Result<(), Error> {
        let offer = self.peer.offer().await.map_err(media_error)?;
        let answer = self.call.start(options, tools, &offer, deadline).await?;
        self.peer.apply_answer(&answer).await.map_err(media_error)?;
        let mut states = self.peer.states();
        loop {
            match *states.borrow_and_update() {
                PeerState::Connected => return Ok(()),
                PeerState::Failed | PeerState::Closed => return Err(Error::Unavailable),
                _ => (),
            }
            // Keep the process pipes flowing while ICE/DTLS completes. Preserve
            // early transcript/tool events, with the signaling queue's bound.
            tokio::select! {
                result = states.changed() => result.map_err(|_| Error::Unavailable)?,
                event = self.call.next() => match event? {
                    Event::Closed => return Err(Error::Unavailable),
                    Event::Other => (),
                    event => {
                        if self.early.len() == 32 { return Err(Error::Unavailable); }
                        self.early.push_back(event);
                    }
                },
            }
        }
    }

    /// Forward a complete 20 ms mono/16 kHz frame. No bridge Opus encoding.
    /// # Errors
    /// Rejects nonactive sessions or the peer's fixed failure category.
    pub fn send(&self, frame: PcmFrame) -> Result<(), Error> {
        if !self.active {
            return Err(Error::Unavailable);
        }
        self.peer.send(frame).map_err(media_error)
    }

    /// Cancellation-safe, fair polling of PCM and app-server events, also while idle.
    /// # Errors
    /// Transport/media failure closes the generation; EOF is never reply completion.
    pub async fn next(&mut self) -> Result<MediaEvent, Error> {
        if !self.active {
            return Err(Error::Unavailable);
        }
        if let Some(event) = self.early.pop_front() {
            return Ok(MediaEvent::Control(event));
        }
        let result = tokio::select! {
            event = self.call.next() => event.map(MediaEvent::Control),
            frame = self.peer.recv() => match frame.map_err(media_error) {
                Ok(Some(frame)) => Ok(MediaEvent::Audio(Box::new(frame))),
                Ok(None) => Err(Error::Unavailable),
                Err(error) => Err(error),
            },
        };
        if result.is_err() || matches!(&result, Ok(MediaEvent::Control(Event::Closed))) {
            self.active = false;
            self.call.fail();
            let _ = self.peer.close().await;
            self.closed = true;
        }
        result
    }

    /// # Errors
    /// Retains the signaling adapter's original-ID and one-answer restrictions.
    pub fn answer_tool(&mut self, id: &RequestId, result: &ToolResult) -> Result<(), Error> {
        if !self.active {
            return Err(Error::Unavailable);
        }
        self.call.answer_tool(id, result)
    }

    #[must_use]
    pub fn dropped_frames(&self) -> DropCounts {
        self.peer.dropped_frames()
    }

    /// Take one already accepted PCM frame without waiting for future media.
    /// A pending receive is a local queue boundary, never remote audio EOF.
    /// # Errors
    /// Closed/failed media is not a successful drain. The host must close on error.
    pub async fn ready_audio(&mut self) -> Result<Option<PcmFrame>, Error> {
        if !self.active {
            return Err(Error::Unavailable);
        }
        let receive = self.peer.recv();
        tokio::pin!(receive);
        std::future::poll_fn(|cx| match std::future::Future::poll(receive.as_mut(), cx) {
            std::task::Poll::Pending => std::task::Poll::Ready(Ok(None)),
            std::task::Poll::Ready(Ok(Some(frame))) => std::task::Poll::Ready(Ok(Some(frame))),
            std::task::Poll::Ready(Ok(None)) => std::task::Poll::Ready(Err(Error::Unavailable)),
            std::task::Poll::Ready(Err(error)) => std::task::Poll::Ready(Err(media_error(error))),
        })
        .await
    }

    /// Await both stop acknowledgement/closed and peer worker/socket release.
    /// # Errors
    /// Cleanup of either side still runs when the other fails.
    pub async fn close(&mut self, deadline: Instant) -> Result<(), Error> {
        if self.closed {
            return Ok(());
        }
        self.early.clear();
        if !self.active {
            if self.started {
                self.call.fail();
            }
            let result = self.peer.close().await.map_err(media_error);
            self.closed = true;
            return result;
        }
        self.active = false;
        let (control, media) = tokio::join!(self.call.stop(deadline), self.peer.close());
        self.closed = true;
        control.and(media.map_err(media_error))
    }
}

fn media_error(error: echo_media::Error) -> Error {
    match error {
        echo_media::Error::Deadline => Error::Deadline,
        echo_media::Error::Audio => Error::Audio,
        echo_media::Error::InvalidSdp => Error::Decode,
        _ => Error::Unavailable,
    }
}

#[cfg(test)]
#[path = "call_media_tests/call_media_tests.rs"]
mod tests;
