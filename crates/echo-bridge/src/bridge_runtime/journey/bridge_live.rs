//! Minimal bridge-owned Live port; production forwards directly to the existing call.
use crate::Error;
use echo_contracts::{ids::DelegationId, live::LiveEvent};
use echo_live::{
    AudioFrame,
    call::{CallError, LiveCall},
};
use std::{future::Future, time::Instant};

pub(in crate::bridge_runtime) trait LivePort: Send {
    fn send_audio(&mut self, frame: AudioFrame, now: Instant) -> impl Future<Output = Result<(), CallError>> + Send;
    fn receive_event(&mut self, deadline: Instant) -> impl Future<Output = Result<LiveEvent, CallError>> + Send;
    fn append_commentary(
        &mut self,
        id: DelegationId,
        text: String,
    ) -> impl Future<Output = Result<(), CallError>> + Send;
    fn pop_audio(&mut self) -> Option<AudioFrame>;
    fn close(&mut self);
}
impl LivePort for LiveCall {
    async fn send_audio(&mut self, frame: AudioFrame, now: Instant) -> Result<(), CallError> {
        Self::send_audio(self, frame, now).await
    }
    async fn receive_event(&mut self, deadline: Instant) -> Result<LiveEvent, CallError> {
        Self::receive_event(self, deadline).await
    }
    async fn append_commentary(&mut self, id: DelegationId, text: String) -> Result<(), CallError> {
        Self::append_commentary(self, id, text).await
    }
    fn pop_audio(&mut self) -> Option<AudioFrame> {
        Self::pop_audio(self)
    }
    fn close(&mut self) {
        Self::close(self);
    }
}

pub(super) async fn send(
    call: &mut impl LivePort,
    packets: Vec<AudioFrame>,
    screen: &super::super::bridge_screen::Screen,
    sent: &mut u64,
) -> Result<(), Error> {
    for packet in packets {
        if screen.muted()? {
            break;
        }
        call.send_audio(packet, tokio::time::Instant::now().into_std()).await?;
        *sent += 1;
        screen.sent()?;
    }
    Ok(())
}
