//! Bounded appendText operations on the existing call and event owner.
use super::{Call, Error, Phase, rpc_error};
use serde_json::json;
use std::{future::Future, task::Poll};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::Instant,
};

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Call<'_, R, W> {
    pub(super) async fn refresh_wake_time(
        &mut self,
        deadline: Instant,
        cancelled: impl Future<Output = ()>,
    ) -> Result<(), Error> {
        let text = crate::bridge_voice::wake_time_context(self.time_zone.as_deref())?;
        self.append_text("developer", &text, deadline, cancelled).await
    }

    pub(super) async fn append_text(
        &mut self,
        role: &str,
        text: &str,
        deadline: Instant,
        cancelled: impl Future<Output = ()>,
    ) -> Result<(), Error> {
        tokio::pin!(cancelled);
        if self.phase != Phase::Active {
            return Err(Error::Unavailable);
        }
        let params = json!({"threadId":self.thread, "role":role, "text":text});
        let id = self
            .io
            .begin_request("thread/realtime/appendText", &params)
            .map_err(rpc_error)?;
        let mut replied = false;
        let result = tokio::select! {
            biased;
            () = cancelled.as_mut() => Err(Error::Control),
            () = tokio::time::sleep_until(deadline) => Err(Error::Deadline),
            result = std::future::poll_fn(|cx| {
                // Poll both cancellation-safe IO operations with one borrower.
                // Project ready errors/closure before admitting audio even if
                // their notification arrived in the same read as the reply.
                loop {
                    let notification = std::pin::pin!(self.io.next_event()).poll(cx);
                    match notification {
                        Poll::Ready(message) => self.defer_event(message.map_err(rpc_error)?)?,
                        Poll::Pending if replied => return Poll::Ready(Ok(())),
                        Poll::Pending => match std::pin::pin!(self.io.wait_reply(&id)).poll(cx) {
                            Poll::Ready(reply) => {
                                reply.map_err(rpc_error)?;
                                replied = true;
                            }
                            Poll::Pending => return Poll::Pending,
                        },
                    }
                }
            }) => result,
        };
        self.io.cancel(&id);
        result
    }
}
