//! Joint signaling/media owner; readiness requires ICE, DTLS and the data channel.
use super::{Call, Event, Options};
use crate::Error;
use echo_codex::{
    DynamicTool,
    codex_io::RpcIo,
    codex_protocol::{RequestId, ToolResult, TurnInterrupt},
};
use echo_media::{DropCounts, MediaPeer, PcmFrame, Peer, PeerState, SetupStage};
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

#[derive(Default, PartialEq, Eq)]
enum ReceiveState {
    #[default]
    Current,
    Retired,
    Resetting,
}

/// One call owns its media peer and exclusively borrows initialized app-server IO.
/// Always await close, then supervisor shutdown, on host failure/cancellation.
pub struct MediaCall<'a, R: AsyncRead + Unpin, W: AsyncWrite + Unpin, P: MediaPeer = Peer> {
    call: Call<'a, R, W>,
    peer: P,
    early: VecDeque<Event>,
    receive_state: ReceiveState,
    started: bool,
    active: bool,
    closed: bool,
    trace: Option<crate::bridge_trace::Trace>,
    stage: &'static str,
    trace_failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl<'a, R: AsyncRead + Unpin, W: AsyncWrite + Unpin, P: MediaPeer> MediaCall<'a, R, W, P> {
    pub(super) fn trace_timeout(&mut self, turn: &str) {
        if let Some(trace) = &mut self.call.tool_trace {
            trace.timeout(turn);
        }
    }

    pub(crate) fn trace_tools(
        &mut self,
        trace: crate::bridge_trace::Trace,
        catalog: &echo_delegation::codex_dispatch::Catalog,
        server: Option<&str>,
    ) {
        self.trace = Some(trace.clone());
        let mut observer = crate::bridge_trace::ToolTrace::new(trace, catalog);
        observer.server(server);
        self.call.tool_trace = Some(observer);
    }

    pub(crate) fn trace_setup(&mut self, trace: crate::bridge_trace::Trace) -> Result<(), Error> {
        let observer_trace = trace.clone();
        let failed = self.trace_failed.clone();
        self.peer
            .observe_setup(std::sync::Arc::new(move |stage| {
                let event = match stage {
                    SetupStage::IceConnected => "call.ice_connected",
                    SetupStage::DataChannelOpen => "call.data_channel_open",
                };
                observer_trace
                    .clone()
                    .record(event, None, serde_json::json!({}))
                    .map_err(|_| {
                        failed.store(true, std::sync::atomic::Ordering::Release);
                        echo_media::Error::Transport
                    })
            }))
            .map_err(media_error)?;
        self.call.setup_trace = Some(trace.clone());
        self.trace = Some(trace);
        Ok(())
    }

    fn record_stage(&mut self, event: &'static str) -> Result<(), Error> {
        if self.trace_failed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(Error::Trace);
        }
        if let Some(trace) = &mut self.call.setup_trace {
            trace.record(event, None, serde_json::json!({}))?;
        }
        Ok(())
    }

    pub fn new(io: &'a mut RpcIo<R, W>, peer: P) -> Self {
        Self {
            call: Call::new(io),
            peer,
            early: VecDeque::new(),
            receive_state: ReceiveState::Current,
            started: false,
            active: false,
            closed: false,
            trace: None,
            stage: "new",
            trace_failed: std::sync::Arc::default(),
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
        let result = if Instant::now() >= deadline {
            Err(Error::Deadline)
        } else {
            timeout_at(deadline, self.negotiate(options, tools, deadline))
                .await
                .unwrap_or(Err(Error::Deadline))
        };
        let diagnostic = if result.is_err() {
            self.diagnostic("call.start_failed")
        } else {
            Ok(())
        };
        if result.is_err() {
            self.call.fail();
            let _ = self.peer.close().await;
            self.closed = true;
        } else {
            self.active = true;
        }
        if result.is_ok() {
            self.stage = "media_ready";
        }
        if self.trace_failed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(Error::Trace);
        }
        result.and(diagnostic)
    }

    async fn negotiate(
        &mut self,
        options: &Options<'_>,
        tools: Vec<DynamicTool>,
        deadline: Instant,
    ) -> Result<(), Error> {
        // Only thread preparation is independent of the offer. Keep the final
        // realtime request behind both results, with no spawned sibling task.
        let mut trace = self.call.setup_trace.clone();
        let (offer, ()) = tokio::try_join!(
            async {
                let offer = self.peer.offer().await.map_err(media_error)?;
                if let Some(trace) = &mut trace {
                    trace.record("call.offer_ready", None, serde_json::json!({}))?;
                }
                Ok(offer)
            },
            self.call.prepare(options, tools, deadline),
        )?;
        self.stage = "signaling_started";
        let answer = self.call.start_prepared(options, &offer, deadline).await?;
        self.stage = "answer_received";
        self.peer.apply_answer(&answer).await.map_err(media_error)?;
        self.record_stage("call.answer_applied")?;
        self.stage = "answer_applied";
        let mut states = self.peer.states();
        loop {
            match *states.borrow_and_update() {
                PeerState::Connected => return self.record_stage("call.live_connected"),
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

    fn diagnostic(&mut self, event: &'static str) -> Result<(), Error> {
        if let Some(trace) = &mut self.trace {
            trace.record(
                event,
                None,
                serde_json::json!({"stage":self.stage, "media_state":format!("{:?}", *self.peer.states().borrow())}),
            )?;
        }
        Ok(())
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

    /// Like `send`, but a full bounded sender refuses the frame instead of
    /// evicting its oldest one. False leaves the frame with the caller.
    /// # Errors
    /// Rejects nonactive sessions or the peer's fixed failure category.
    pub fn try_send(&self, frame: PcmFrame) -> Result<bool, Error> {
        if !self.active {
            return Err(Error::Unavailable);
        }
        self.peer.try_send(frame).map_err(media_error)
    }

    /// Resolve once `try_send` can accept a frame, or the peer ended.
    pub fn send_ready(&self) -> impl Future<Output = ()> + Send + use<R, W, P> {
        self.peer.send_ready()
    }

    /// Cancellation-safe, fair polling of PCM and app-server events, also while idle.
    /// # Errors
    /// Transport/media failure closes the generation; EOF is never reply completion.
    pub async fn next(&mut self) -> Result<MediaEvent, Error> {
        if !self.active {
            return Err(Error::Unavailable);
        }
        if self.receive_state == ReceiveState::Resetting {
            self.finish_receive_reset().await?;
        }
        if let Some(event) = self.early.pop_front() {
            return self.accept_media(MediaEvent::Control(event)).await;
        }
        let (result, source) = tokio::select! {
            event = self.call.next() => (event.map(MediaEvent::Control), "call.control_failed"),
            frame = self.peer.recv() => (match frame.map_err(media_error) {
                Ok(Some(frame)) => Ok(MediaEvent::Audio(Box::new(frame))),
                Ok(None) => Err(Error::Unavailable),
                Err(error) => Err(error),
            }, "call.media_failed"),
        };
        if result.is_err() || matches!(&result, Ok(MediaEvent::Control(Event::Closed))) {
            let diagnostic = self.diagnostic(source);
            self.active = false;
            if source == "call.media_failed"
                && let Some(trace) = &mut self.call.tool_trace
            {
                let _ = trace.close("failed", Some("media_failed"));
            }
            self.call.fail();
            let _ = self.peer.close().await;
            self.closed = true;
            return result.and_then(|event| diagnostic.map(|()| event));
        }
        self.accept_media(result?).await
    }

    pub(crate) fn retire_audio(&mut self) {
        if self.receive_state == ReceiveState::Current {
            self.receive_state = ReceiveState::Retired;
        }
    }

    async fn finish_receive_reset(&mut self) -> Result<(), Error> {
        self.peer.reset_receive().await.map_err(media_error)?;
        self.receive_state = ReceiveState::Current;
        self.record_stage("call.receive_reset")
    }

    async fn accept_media(&mut self, event: MediaEvent) -> Result<MediaEvent, Error> {
        if self.receive_state == ReceiveState::Retired
            && matches!(&event, MediaEvent::Control(Event::Transcript { role, done: false, .. }) if role == "assistant")
        {
            let MediaEvent::Control(event) = event else {
                return Err(Error::Input);
            };
            // Retain the boundary across cancelled next() polls. Replacement
            // control and PCM cannot escape until the receive task acknowledges.
            self.early.push_front(event);
            self.receive_state = ReceiveState::Resetting;
            self.finish_receive_reset().await?;
            return self
                .early
                .pop_front()
                .map(MediaEvent::Control)
                .ok_or(Error::Unavailable);
        }
        Ok(event)
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
    pub fn thread_id(&self) -> Option<&str> {
        self.call.thread_id()
    }

    /// # Errors
    /// Retains the signaling owner's lifecycle and correlation restrictions.
    pub fn interrupt(&mut self, params: &TurnInterrupt, deadline: Instant) -> Result<(), Error> {
        if !self.active {
            return Err(Error::Unavailable);
        }
        self.call.interrupt(params, deadline)
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

impl<'a, R: AsyncRead + Unpin, W: AsyncWrite + Unpin, P: MediaPeer> crate::bridge_voice::VoicePort
    for MediaCall<'a, R, W, P>
{
    fn send(&self, frame: PcmFrame) -> Result<(), Error> {
        Self::send(self, frame)
    }
    fn try_send(&self, frame: PcmFrame) -> Result<bool, Error> {
        Self::try_send(self, frame)
    }
    fn uplink_ready(&self) -> impl Future<Output = ()> + use<'a, R, W, P> {
        self.send_ready()
    }
    async fn next(&mut self) -> Result<MediaEvent, Error> {
        Self::next(self).await
    }
    async fn ready_audio(&mut self) -> Result<Option<PcmFrame>, Error> {
        Self::ready_audio(self).await
    }
    fn retire_audio(&mut self) {
        Self::retire_audio(self);
    }
    fn rtp_lost(&self) -> u64 {
        self.dropped_frames().lost
    }
}
