//! Caller-driven asynchronous JSONL transport. No detached tasks or hidden queues.
use crate::{
    codex_codec::Decoder,
    codex_protocol::{Message, RequestId},
    codex_rpc::{Limits, RpcError, RpcState},
};
use serde::Serialize;
use serde_json::Value;
use std::{collections::VecDeque, future::pending};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    time::{Instant, sleep_until},
};

struct Frame {
    id: Option<RequestId>,
    bytes: Vec<u8>,
    offset: usize,
    deadline: Instant,
}

enum Progress {
    Read(std::io::Result<usize>),
    Write(std::io::Result<usize>),
    Flushed(std::io::Result<()>),
    Timer,
}

/// A single initialized-or-uninitialized process generation's stream driver.
/// Call `progress`, `wait_reply` or `next_event` to drive both IO directions and
/// deadlines. OS pipes provide backpressure while the caller is idle.
/// Dropping a wait future preserves partial input/output and its admitted ID;
/// the caller must explicitly `cancel` that ID to abandon the request.
pub struct RpcIo<R, W> {
    transport: Option<(R, W)>,
    state: RpcState,
    decoder: Decoder,
    outgoing: VecDeque<Frame>,
    limits: Limits,
    epoch: Instant,
    terminal: Option<RpcError>,
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> RpcIo<R, W> {
    /// # Errors
    /// All byte, queue and timeout bounds must be nonzero.
    pub fn new(reader: R, writer: W, limits: Limits) -> Result<Self, RpcError> {
        Ok(Self {
            transport: Some((reader, writer)),
            state: RpcState::new(limits)?,
            decoder: Decoder::new(limits.line_bytes)?,
            outgoing: VecDeque::new(),
            limits,
            epoch: Instant::now(),
            terminal: None,
        })
    }

    /// Admit a call without blocking. IO starts on the next driver poll.
    /// # Errors
    /// Reject disconnected sessions, excess admission and oversized frames.
    pub fn begin_request(&mut self, method: &str, params: &impl Serialize) -> Result<RequestId, RpcError> {
        let deadline = self.admit_output()?;
        let (id, bytes) = self.state.request(method, params, self.epoch.elapsed())?;
        self.enqueue(Some(id.clone()), bytes, deadline);
        Ok(id)
    }

    /// Queue a method-only notification, e.g. `initialized`.
    /// # Errors
    /// Output admission uses the same finite frame count as pending calls.
    pub fn notify(&mut self, method: &str) -> Result<(), RpcError> {
        let deadline = self.admit_output()?;
        let bytes = self.state.notify(method)?;
        self.enqueue(None, bytes, deadline);
        Ok(())
    }

    /// Queue one response using the original server request ID.
    /// # Errors
    /// Rejected admission/serialization leaves the server request answerable.
    pub fn respond(&mut self, id: &RequestId, result: &impl Serialize) -> Result<(), RpcError> {
        let deadline = self.admit_output()?;
        let bytes = self.state.respond(id, result)?;
        self.enqueue(None, bytes, deadline);
        Ok(())
    }

    /// Release admission and discard a wholly unsent request. A partial frame
    /// must finish to preserve framing; any late response is ignored.
    pub fn cancel(&mut self, id: &RequestId) -> bool {
        self.outgoing
            .retain(|frame| frame.id.as_ref() != Some(id) || frame.offset != 0);
        self.state.cancel(id)
    }

    /// Wait for one admitted ID while servicing every inbound/outbound frame.
    /// # Errors
    /// Return the matching remote error, deadline or transport failure once.
    pub async fn wait_reply(&mut self, id: &RequestId) -> Result<Value, RpcError> {
        loop {
            self.state.expire(self.epoch.elapsed());
            if let Some(reply) = self.state.take_reply(id)? {
                self.cancel(id);
                return reply;
            }
            // A failed pump settles all pending IDs. Read that settled reply on
            // the next iteration, preserving an already completed response.
            let _ = self.progress().await;
        }
    }

    /// Drain accepted events before reporting a terminal connection failure.
    /// # Errors
    /// Overflow is explicit; accepted notifications are never silently lost.
    pub async fn next_event(&mut self) -> Result<Message, RpcError> {
        loop {
            if let Some(event) = self.state.next_event() {
                return Ok(event);
            }
            self.connected()?;
            let _ = self.progress().await;
        }
    }

    /// Flush queued output while still servicing input and request deadlines.
    /// # Errors
    /// A stalled/failed write terminates this generation, including pending IDs.
    pub async fn flush(&mut self) -> Result<(), RpcError> {
        self.connected()?;
        while !self.outgoing.is_empty() {
            self.progress().await?;
        }
        Ok(())
    }

    /// Perform one cancellation-safe read, write, flush or deadline step.
    /// # Errors
    /// Malformed/oversized input, EOF, IO failure and queue overflow close IO.
    pub async fn progress(&mut self) -> Result<(), RpcError> {
        self.connected()?;
        if let Err(error) = self.expire() {
            self.close(error);
            return Err(error);
        }
        let deadline = self.next_deadline();
        let Some((reader, writer)) = &mut self.transport else {
            return Err(RpcError::Disconnected);
        };
        let mut bytes = [0; 4096];
        let step = tokio::select! {
            result = reader.read(&mut bytes) => Progress::Read(result),
            result = write_step(writer, self.outgoing.front()) => result,
            () = wait_until(deadline) => Progress::Timer,
        };
        let result = match step {
            Progress::Read(Ok(0)) => Err(self.decoder.eof()),
            Progress::Read(Ok(count)) => self.receive(&bytes[..count]),
            Progress::Write(Ok(0) | Err(_)) | Progress::Read(Err(_)) | Progress::Flushed(Err(_)) => {
                Err(RpcError::Disconnected)
            }
            Progress::Write(Ok(count)) => {
                if let Some(frame) = self.outgoing.front_mut() {
                    frame.offset += count;
                }
                Ok(())
            }
            Progress::Flushed(Ok(())) => {
                self.outgoing.pop_front();
                Ok(())
            }
            Progress::Timer => self.expire(),
        };
        if let Err(error) = result {
            self.close(error);
        }
        result
    }

    /// Close both stream halves, fail pending calls and discard unsent output.
    /// Accepted events and earlier replies remain drainable; nothing is replayed.
    pub fn close(&mut self, reason: RpcError) {
        if self.terminal.is_none() {
            self.terminal = Some(reason);
            self.state.disconnect(reason);
            self.transport.take();
            self.outgoing.clear();
        }
    }

    fn receive(&mut self, bytes: &[u8]) -> Result<(), RpcError> {
        for byte in bytes {
            if let Some(message) = self.decoder.push(*byte)? {
                self.state.receive(message, self.epoch.elapsed())?;
            }
        }
        Ok(())
    }

    fn expire(&mut self) -> Result<(), RpcError> {
        self.state.expire(self.epoch.elapsed());
        if self
            .outgoing
            .front()
            .is_some_and(|frame| frame.deadline <= Instant::now())
        {
            return Err(RpcError::Deadline);
        }
        Ok(())
    }

    fn connected(&self) -> Result<(), RpcError> {
        self.terminal.map_or(Ok(()), Err)
    }

    fn admit_output(&self) -> Result<Instant, RpcError> {
        self.connected()?;
        if self.outgoing.len() == self.limits.pending_requests {
            return Err(RpcError::PendingFull);
        }
        Instant::now()
            .checked_add(self.limits.request_timeout)
            .ok_or(RpcError::InvalidLimits)
    }

    fn enqueue(&mut self, id: Option<RequestId>, bytes: Vec<u8>, deadline: Instant) {
        self.outgoing.push_back(Frame {
            id,
            bytes,
            offset: 0,
            deadline,
        });
    }

    fn next_deadline(&self) -> Option<Instant> {
        let request = self
            .state
            .next_deadline()
            .and_then(|elapsed| self.epoch.checked_add(elapsed));
        request
            .into_iter()
            .chain(self.outgoing.front().map(|frame| frame.deadline))
            .min()
    }
}

async fn wait_until(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        sleep_until(deadline).await;
    } else {
        pending::<()>().await;
    }
}

async fn write_step<W: AsyncWrite + Unpin>(writer: &mut W, frame: Option<&Frame>) -> Progress {
    let Some(frame) = frame else {
        return pending().await;
    };
    if frame.offset == frame.bytes.len() {
        Progress::Flushed(writer.flush().await)
    } else {
        Progress::Write(writer.write(&frame.bytes[frame.offset..]).await)
    }
}

#[cfg(test)]
#[path = "codex_io_tests.rs"]
mod tests;
