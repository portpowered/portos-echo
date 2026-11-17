//! Bridge-owned call signaling on one initialized app-server generation.
//! The host owns the supervisor and media; no HTTP call or delegated model here.
mod call_events;
mod call_media;
pub use call_media::{MediaCall, MediaEvent};
#[cfg(test)]
mod call_failure_tests;
#[cfg(test)]
mod call_tests;

use crate::Error;
use echo_codex::{
    DynamicTool, ThreadSettings,
    codex_io::RpcIo,
    codex_protocol::{Message, RealtimeStart, RequestId, ThreadStartResult, ToolCall, ToolResult},
    codex_rpc::RpcError,
};
use serde_json::json;
use std::collections::VecDeque;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::{Instant, timeout_at},
};

/// Payloads intentionally have no Debug/Display implementation.
pub enum Event {
    Transcript { role: String, text: String, done: bool },
    ReplyDone { text: String },
    Tool { id: RequestId, call: ToolCall },
    Closed,
    Other,
}

/// Call configuration, distinct from the delegated thread's optional model.
pub struct Options<'a> {
    pub voice: &'a str,
    pub instructions: &'a str,
    pub model: Option<&'a str>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Fresh,
    Starting,
    Active,
    Closed,
}

/// Exclusive IO borrow prevents competing readers and cross-generation reuse.
/// Call stop before releasing a healthy call. Abandonment closes this generation;
/// the host must always await supervisor shutdown, including on cancellation.
pub struct Call<'a, R: AsyncRead + Unpin, W: AsyncWrite + Unpin> {
    io: &'a mut RpcIo<R, W>,
    phase: Phase,
    thread: Option<String>,
    early: VecDeque<Event>,
}

impl<'a, R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Call<'a, R, W> {
    pub fn new(io: &'a mut RpcIo<R, W>) -> Self {
        Self {
            io,
            phase: Phase::Fresh,
            thread: None,
            early: VecDeque::new(),
        }
    }

    /// Start exactly once and return the matching answer for the caller's media.
    /// The caller supplies an actual media offer, never a fabricated runtime SDP.
    /// # Errors
    /// Invalid state/input is rejected before admission. RPC/notification failure
    /// or the absolute deadline closes this IO generation.
    pub async fn start(
        &mut self,
        options: &Options<'_>,
        tools: Vec<DynamicTool>,
        offer: &str,
        deadline: Instant,
    ) -> Result<String, Error> {
        if self.phase != Phase::Fresh || offer.trim().is_empty() {
            return Err(Error::Input);
        }
        self.phase = Phase::Starting;
        let result = timeout_at(deadline, self.negotiate(options, tools, offer))
            .await
            .unwrap_or(Err(Error::Deadline));
        match result {
            Ok(answer) => {
                self.phase = Phase::Active;
                Ok(answer)
            }
            Err(error) => {
                self.fail();
                Err(error)
            }
        }
    }

    async fn negotiate(
        &mut self,
        options: &Options<'_>,
        tools: Vec<DynamicTool>,
        offer: &str,
    ) -> Result<String, Error> {
        let mut params = serde_json::to_value(ThreadSettings::new(tools)).map_err(|_| Error::Decode)?;
        params["baseInstructions"] = options.instructions.into();
        if let Some(model) = options.model {
            params["model"] = model.into();
        }
        let request = self.io.begin_request("thread/start", &params).map_err(rpc_error)?;
        let response: ThreadStartResult =
            serde_json::from_value(self.io.wait_reply(&request).await.map_err(rpc_error)?)
                .map_err(|_| Error::Decode)?;
        let thread = response.thread.id;
        if thread.is_empty() || thread.chars().any(char::is_control) {
            return Err(Error::Decode);
        }
        self.thread = Some(thread.clone());
        let mut params = serde_json::to_value(RealtimeStart::new(&thread, offer)).map_err(|_| Error::Decode)?;
        params["voice"] = options.voice.into();
        params["prompt"] = options.instructions.into();
        let request = self
            .io
            .begin_request("thread/realtime/start", &params)
            .map_err(rpc_error)?;
        self.io.wait_reply(&request).await.map_err(rpc_error)?;
        self.answer().await
    }

    async fn answer(&mut self) -> Result<String, Error> {
        loop {
            let message = self.io.next_event().await.map_err(rpc_error)?;
            if let Message::Notification { method, params, .. } = &message
                && method == "thread/realtime/sdp"
                && params["threadId"].as_str() == self.thread.as_deref()
            {
                return params["sdp"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_owned)
                    .ok_or(Error::Decode);
            }
            let event = self.project(message)?;
            if matches!(event, Event::Closed) {
                return Err(Error::Unavailable);
            }
            if !matches!(event, Event::Other) {
                if self.early.len() == 32 {
                    return Err(Error::Unavailable);
                }
                self.early.push_back(event);
            }
        }
    }

    /// Drive both IO directions even between wakes; no hidden notification task.
    /// Assistant transcript/done is a signal, not proof that media has drained.
    /// # Errors
    /// EOF, matching realtime error, malformed event or invalid lifecycle state.
    pub async fn next(&mut self) -> Result<Event, Error> {
        if self.phase != Phase::Active {
            return Err(Error::Unavailable);
        }
        if let Some(event) = self.early.pop_front() {
            return Ok(event);
        }
        let result = match self.io.next_event().await {
            Ok(message) => self.project(message),
            Err(error) => Err(rpc_error(error)),
        };
        if result.is_err() {
            self.fail();
        }
        result
    }

    /// Queue the authorized dispatcher's result under its original server ID.
    /// The next IO poll drives delivery; this adapter never executes the tool.
    /// # Errors
    /// Reject inactive calls, unknown or already answered IDs and full output.
    pub fn answer_tool(&mut self, id: &RequestId, result: &ToolResult) -> Result<(), Error> {
        if self.phase != Phase::Active {
            return Err(Error::Unavailable);
        }
        self.io.respond(id, result).map_err(rpc_error)
    }

    /// Stop once. A stopped call cannot start or consume more events.
    /// # Errors
    /// Deadline/transport errors close IO; host cleanup must still reap the child.
    pub async fn stop(&mut self, deadline: Instant) -> Result<(), Error> {
        if self.phase == Phase::Closed {
            return Ok(());
        }
        if self.phase != Phase::Active {
            self.fail();
            return Err(Error::Unavailable);
        }
        self.phase = Phase::Starting;
        let result = timeout_at(deadline, async {
            let id = self
                .io
                .begin_request("thread/realtime/stop", &json!({"threadId":self.thread}))
                .map_err(rpc_error)?;
            self.io.wait_reply(&id).await.map_err(rpc_error)?;
            loop {
                let message = self.io.next_event().await.map_err(rpc_error)?;
                if matches!(self.project(message)?, Event::Closed) {
                    return Ok(());
                }
            }
        })
        .await
        .unwrap_or(Err(Error::Deadline));
        self.early.clear();
        if result.is_err() {
            self.fail();
        } else {
            self.phase = Phase::Closed;
        }
        result
    }

    fn fail(&mut self) {
        self.io.close(RpcError::Disconnected);
        self.phase = Phase::Closed;
        self.early.clear();
    }
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Drop for Call<'_, R, W> {
    fn drop(&mut self) {
        if self.phase != Phase::Closed && self.phase != Phase::Fresh {
            self.io.close(RpcError::Disconnected);
        }
    }
}

fn rpc_error(error: RpcError) -> Error {
    match error {
        RpcError::Deadline => Error::Deadline,
        RpcError::Malformed => Error::Decode,
        _ => Error::Unavailable,
    }
}
