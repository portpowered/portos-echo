//! Joint signaling/media owner; readiness requires ICE, DTLS and the data channel.
use super::{Call, Event, Options};
use crate::{Error, bridge_world::CallPort};
use echo_codex::{
    DynamicTool,
    codex_io::RpcIo,
    codex_protocol::{RequestId, ToolResult, TurnInterrupt},
};
use echo_core_api::{CallCommand, CallEvent, CallPhase, CallReceive, CallSetupStage, CallView};
use echo_media::{DropCounts, MediaPeer, PcmFrame, Peer, PeerState, SetupStage};
use std::collections::VecDeque;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::{Instant, timeout_at},
};

pub(crate) trait RetainedCall: crate::bridge_voice::VoicePort {
    fn refresh_wake_time(
        &mut self,
        deadline: Instant,
        cancelled: impl std::future::Future<Output = ()>,
    ) -> impl std::future::Future<Output = Result<(), Error>>;
}

/// Media and signaling remain independently observable. `ReplyDone` does not drain PCM.
pub struct MediaEvent {
    control: Option<Event>,
    audio: Option<PcmFrame>,
}

impl MediaEvent {
    #[must_use]
    pub fn control(event: Event) -> Self {
        Self {
            control: Some(event),
            audio: None,
        }
    }

    #[must_use]
    pub fn audio(frame: PcmFrame) -> Self {
        Self {
            control: None,
            audio: Some(frame),
        }
    }

    #[must_use]
    pub fn control_event(&self) -> Option<&Event> {
        self.control.as_ref()
    }

    #[must_use]
    pub fn into_parts(self) -> (Option<Event>, Option<PcmFrame>) {
        (self.control, self.audio)
    }
}

/// One call owns its media peer and exclusively borrows initialized app-server IO.
/// Always await close, then supervisor shutdown, on host failure/cancellation.
pub struct MediaCall<'a, R: AsyncRead + Unpin, W: AsyncWrite + Unpin, P: MediaPeer = Peer> {
    call: Call<'a, R, W>,
    peer: P,
    early: VecDeque<Event>,
    state: CallPort,
    trace: Option<crate::bridge_trace::Trace>,
    trace_failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl<'a, R: AsyncRead + Unpin, W: AsyncWrite + Unpin, P: MediaPeer> MediaCall<'a, R, W, P> {
    pub(crate) async fn append_endpoint_text(&mut self, text: &str, deadline: Instant) -> Result<(), Error> {
        self.call
            .append_text("user", text, deadline, std::future::pending())
            .await
    }
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
            state: CallPort::attach(),
            trace: None,
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
        let (action, _) = self.state.send(|call| CallEvent::OpenRequested { call }).into_parts();
        if !matches!(action, Some(CallCommand::OpenPeer(_))) {
            return Err(Error::Input);
        }
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
            self.fail().await;
        } else {
            self.state.send(|call| CallEvent::PeerConnected { call });
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
        self.setup_stage(CallSetupStage::SignalingStarted);
        let answer = self.call.start_prepared(options, &offer, deadline).await?;
        self.setup_stage(CallSetupStage::AnswerReceived);
        self.peer.apply_answer(&answer).await.map_err(media_error)?;
        self.record_stage("call.answer_applied")?;
        self.setup_stage(CallSetupStage::AnswerApplied);
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
            let stage = self.state.view().unwrap_or_default().setup_stage.diagnostic();
            trace.record(
                event,
                None,
                serde_json::json!({"stage":stage, "media_state":format!("{:?}", *self.peer.states().borrow())}),
            )?;
        }
        Ok(())
    }

    fn setup_stage(&self, stage: CallSetupStage) {
        self.state.send(|call| CallEvent::SetupStage { call, stage });
    }

    fn active(&self) -> Result<CallView, Error> {
        self.state
            .view()
            .filter(|view| view.phase == CallPhase::Connected)
            .ok_or(Error::Unavailable)
    }

    async fn fail(&mut self) {
        let (action, _) = self.state.send(|call| CallEvent::PeerFailed { call }).into_parts();
        if matches!(action, Some(CallCommand::ClosePeer(_))) {
            self.call.fail();
            let _ = self.peer.close().await;
            self.state.send(|call| CallEvent::PeerClosed { call });
        }
    }

    /// Forward a complete 20 ms mono/16 kHz frame. No bridge Opus encoding.
    /// # Errors
    /// Rejects nonactive sessions or the peer's fixed failure category.
    pub fn send(&self, frame: PcmFrame) -> Result<(), Error> {
        self.active()?;
        self.peer.send(frame).map_err(media_error)
    }

    /// Like `send`, but a full bounded sender refuses the frame instead of
    /// evicting its oldest one. False leaves the frame with the caller.
    /// # Errors
    /// Rejects nonactive sessions or the peer's fixed failure category.
    pub fn try_send(&self, frame: PcmFrame) -> Result<bool, Error> {
        self.active()?;
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
        let view = self.active()?;
        if view.receive == CallReceive::Resetting {
            self.finish_receive_reset(view.receive_generation).await?;
        }
        if let Some(event) = self.early.pop_front() {
            return self.accept_media(MediaEvent::control(event)).await;
        }
        let (result, source) = tokio::select! {
            event = self.call.next() => (event.map(MediaEvent::control), "call.control_failed"),
            frame = self.peer.recv() => (
                frame.map_err(media_error)
                    .and_then(|frame| frame.ok_or(Error::Unavailable))
                    .and_then(|frame| remote_audio(&self.state, view.receive_generation, &frame))
                    .map(MediaEvent::audio),
                "call.media_failed",
            ),
        };
        if result.is_err()
            || matches!(
                result.as_ref().ok().and_then(MediaEvent::control_event),
                Some(Event::Closed)
            )
        {
            let diagnostic = self.diagnostic(source);
            if source == "call.media_failed"
                && let Some(trace) = &mut self.call.tool_trace
            {
                let _ = trace.close("failed", Some("media_failed"));
            }
            self.fail().await;
            return result.and_then(|event| diagnostic.map(|()| event));
        }
        self.accept_media(result?).await
    }

    pub(crate) fn retire_audio(&mut self) {
        self.state.send(|call| CallEvent::RetireReceive { call });
    }

    async fn finish_receive_reset(&mut self, generation: u64) -> Result<(), Error> {
        self.peer.reset_receive().await.map_err(media_error)?;
        self.state.send(|call| CallEvent::ReceiveReset { call, generation });
        self.record_stage("call.receive_reset")
    }

    async fn accept_media(&mut self, mut event: MediaEvent) -> Result<MediaEvent, Error> {
        if !matches!(event.control_event(), Some(Event::Transcript { role, done: false, .. }) if role == "assistant") {
            return Ok(event);
        }
        let (action, _) = self
            .state
            .send(|call| CallEvent::BumpReceiveGeneration { call })
            .into_parts();
        let Some(CallCommand::ResetReceive { generation, .. }) = action else {
            return Ok(event);
        };
        // Retain the boundary across cancelled next() polls. Replacement
        // control and PCM cannot escape until the receive task acknowledges.
        self.early.push_front(event.control.take().ok_or(Error::Input)?);
        self.finish_receive_reset(generation).await?;
        self.early
            .pop_front()
            .map(MediaEvent::control)
            .ok_or(Error::Unavailable)
    }

    /// # Errors
    /// Retains the signaling adapter's original-ID and one-answer restrictions.
    pub fn answer_tool(&mut self, id: &RequestId, result: &ToolResult) -> Result<(), Error> {
        self.active()?;
        self.call.answer_tool(id, result)
    }

    pub(super) async fn flush_tool_answer(&mut self) -> Result<(), Error> {
        self.call.flush_tool_answer().await
    }

    #[must_use]
    pub fn thread_id(&self) -> Option<&str> {
        self.call.thread_id()
    }

    /// # Errors
    /// Retains the signaling owner's lifecycle and correlation restrictions.
    pub fn interrupt(&mut self, params: &TurnInterrupt, deadline: Instant) -> Result<(), Error> {
        self.active()?;
        self.call.interrupt(params, deadline)
    }

    /// # Errors
    /// As `interrupt`; a missed acknowledgement never closes the call.
    pub fn interrupt_after_end(&mut self, params: &TurnInterrupt, deadline: Instant) -> Result<(), Error> {
        self.active()?;
        self.call.interrupt_after_end(params, deadline)
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
        let view = self.active()?;
        if view.receive == CallReceive::Resetting {
            return Ok(None);
        }
        let state = &self.state;
        let receive = self.peer.recv();
        tokio::pin!(receive);
        std::future::poll_fn(|cx| match std::future::Future::poll(receive.as_mut(), cx) {
            std::task::Poll::Pending => std::task::Poll::Ready(Ok(None)),
            std::task::Poll::Ready(Ok(Some(frame))) => {
                std::task::Poll::Ready(remote_audio(state, view.receive_generation, &frame).map(Some))
            }
            std::task::Poll::Ready(Ok(None)) => std::task::Poll::Ready(Err(Error::Unavailable)),
            std::task::Poll::Ready(Err(error)) => std::task::Poll::Ready(Err(media_error(error))),
        })
        .await
    }

    /// Await both stop acknowledgement/closed and peer worker/socket release.
    /// # Errors
    /// Cleanup of either side still runs when the other fails.
    pub async fn close(&mut self, deadline: Instant) -> Result<(), Error> {
        let Some(view) = self.state.view() else {
            return self.peer.close().await.map_err(media_error);
        };
        if view.phase == CallPhase::Closed {
            return Ok(());
        }
        self.early.clear();
        let (action, _) = self.state.send(|call| CallEvent::CloseRequested { call }).into_parts();
        let (control, media) =
            if view.phase == CallPhase::Connected && matches!(action, Some(CallCommand::ClosePeer(_))) {
                tokio::join!(self.call.stop(deadline), self.peer.close())
            } else {
                if view.phase != CallPhase::Fresh {
                    self.call.fail();
                }
                (Ok(()), self.peer.close().await)
            };
        self.state.send(|call| CallEvent::PeerClosed { call });
        control.and(media.map_err(media_error))
    }
}

fn remote_audio(state: &CallPort, generation: u64, frame: &PcmFrame) -> Result<PcmFrame, Error> {
    let (action, frame) = state
        .send(|call| CallEvent::RemoteAudioFrame {
            call,
            generation,
            frame,
        })
        .into_parts();
    if matches!(action, Some(CallCommand::DeliverRemoteAudio { .. })) {
        return frame.ok_or(Error::Unavailable);
    }
    Err(Error::Unavailable)
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

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin, P: MediaPeer> RetainedCall for MediaCall<'_, R, W, P> {
    async fn refresh_wake_time(
        &mut self,
        deadline: Instant,
        cancelled: impl std::future::Future<Output = ()>,
    ) -> Result<(), Error> {
        self.call.refresh_wake_time(deadline, cancelled).await
    }
}

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
