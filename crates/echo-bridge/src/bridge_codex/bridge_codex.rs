//! Bridge-owned call signaling on one initialized app-server generation.
//! The host owns the supervisor and media; no HTTP call or delegated model here.
mod call_audio;
mod call_events;
mod call_host;
mod call_media;
mod call_text;
mod call_tools;
pub use call_audio::{CaptureAudio, ReplyAudio};
pub use call_host::{HostOptions, with_host, with_host_options};
pub(crate) use call_media::RetainedCall;
pub use call_media::{MediaCall, MediaEvent};
pub(crate) use call_tools::MuseTool;
pub use call_tools::{SessionCommand, SessionRequest, ToolBudget, ToolSession};
#[cfg(test)]
mod call_failure_tests;
#[cfg(test)]
mod call_tests;
#[cfg(test)]
mod call_turn_tests;

use crate::Error;
use echo_codex::{
    DynamicTool, ThreadSettings,
    codex_io::RpcIo,
    codex_protocol::{Message, RealtimeStart, RequestId, ThreadStartResult, ToolCall, ToolResult, TurnInterrupt},
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
    Transcript {
        role: String,
        text: String,
        done: bool,
    },
    ReplyDone {
        text: String,
    },
    Tool {
        id: RequestId,
        call: ToolCall,
    },
    TurnStarted {
        thread: String,
        turn: String,
    },
    /// `answered` is false when the delegated runner finished without any
    /// final text, so no hand-back will be spoken for this turn.
    TurnCompleted {
        thread: String,
        turn: String,
        answered: bool,
    },
    ReplyHandoff(echo_contracts::ids::DelegationId),
    SpeechStarted,
    SpeechStopped,
    Session(SessionRequest),
    Closed,
    Other,
}

/// Call media and thread profile; admission consumes the zone as instruction text.
#[derive(Clone, Copy)]
pub struct Options<'a> {
    pub voice: Option<&'a str>,
    /// The delegated thread's base instructions; never sent as the Live prompt.
    pub instructions: &'a str,
    /// The GPT-Live prompt; never sent as the delegated base instructions.
    pub prompt: &'a str,
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub speed: echo_config::DelegateSpeed,
    pub time_zone: Option<&'a str>,
    pub web_search: echo_config::WebSearch,
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
    time_zone: Option<String>,
    early: VecDeque<Event>,
    /// Pending interrupt, deadline, and whether a missed ack is tolerated.
    interrupt: Option<(RequestId, Instant, bool)>,
    interrupted: Option<TurnInterrupt>,
    handoffs: std::collections::HashSet<String>,
    answered: Option<String>,
    tool_trace: Option<crate::bridge_trace::ToolTrace>,
    setup_trace: Option<crate::bridge_trace::Trace>,
}

impl<'a, R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Call<'a, R, W> {
    pub fn new(io: &'a mut RpcIo<R, W>) -> Self {
        Self {
            io,
            phase: Phase::Fresh,
            thread: None,
            time_zone: None,
            early: VecDeque::new(),
            interrupt: None,
            interrupted: None,
            handoffs: std::collections::HashSet::new(),
            answered: None,
            tool_trace: None,
            setup_trace: None,
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
        self.prepare(options, tools, deadline).await?;
        self.start_prepared(options, offer, deadline).await
    }

    async fn prepare(
        &mut self,
        options: &Options<'_>,
        tools: Vec<DynamicTool>,
        deadline: Instant,
    ) -> Result<(), Error> {
        if self.phase != Phase::Fresh {
            return Err(Error::Input);
        }
        self.phase = Phase::Starting;
        let result = timeout_at(deadline, self.prepare_thread(options, tools))
            .await
            .unwrap_or(Err(Error::Deadline));
        if result.is_err() {
            self.fail();
        }
        result
    }

    async fn start_prepared(&mut self, options: &Options<'_>, offer: &str, deadline: Instant) -> Result<String, Error> {
        if self.phase != Phase::Starting || self.thread.is_none() || offer.trim().is_empty() {
            self.fail();
            return Err(Error::Input);
        }
        let result = timeout_at(deadline, self.negotiate(options, offer))
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

    async fn prepare_thread(&mut self, options: &Options<'_>, tools: Vec<DynamicTool>) -> Result<(), Error> {
        let mut params = serde_json::to_value(ThreadSettings::new(tools).with_web_search(options.web_search.as_str()))
            .map_err(|_| Error::Decode)?;
        params["baseInstructions"] = options.instructions.into();
        if let Some(model) = options.model {
            params["model"] = model.into();
        }
        if let Some(effort) = options.effort {
            params["config"]["model_reasoning_effort"] = effort.into();
        }
        // Present null clears an inherited tier and explicitly selects standard routing.
        let requested = options.speed.service_tier();
        params["serviceTier"] = json!(requested);
        let request = self.io.begin_request("thread/start", &params).map_err(rpc_error)?;
        let response: ThreadStartResult =
            serde_json::from_value(self.io.wait_reply(&request).await.map_err(rpc_error)?)
                .map_err(|_| Error::Decode)?;
        let thread = response.thread.id;
        if thread.is_empty() || thread.chars().any(char::is_control) {
            return Err(Error::Decode);
        }
        self.record_tier(requested, response.service_tier.as_deref())?;
        self.thread = Some(thread);
        self.time_zone = options.time_zone.map(str::to_owned);
        Ok(())
    }

    /// The app server serves a tier the model or account cannot use as standard,
    /// without an error. A faster request that comes back as another tier is kept
    /// non-fatal and traced; an answer without the field says nothing.
    fn record_tier(&mut self, requested: Option<&str>, effective: Option<&str>) -> Result<(), Error> {
        let (Some(requested), Some(effective), Some(trace)) = (requested, effective, &mut self.setup_trace) else {
            return Ok(());
        };
        if requested == effective {
            return Ok(());
        }
        // A tier is a short lowercase name; anything else is reported only as its absence.
        let effective = (effective.len() <= 32 && effective.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
            .then_some(effective);
        trace.record(
            "call.service_tier_downgraded",
            None,
            json!({"requested": requested, "effective": effective}),
        )
    }

    async fn negotiate(&mut self, options: &Options<'_>, offer: &str) -> Result<String, Error> {
        let thread = self.thread.as_deref().ok_or(Error::Input)?;
        let mut params = serde_json::to_value(RealtimeStart::new(thread, offer)).map_err(|_| Error::Decode)?;
        if let Some(voice) = call_host::admitted_voice(options.voice) {
            params["voice"] = voice.into();
        }
        params["prompt"] = options.prompt.into();
        if let Some(items) = live_history(options.prompt) {
            params["initialItems"] = items;
        }
        let request = self
            .io
            .begin_request("thread/realtime/start", &params)
            .map_err(rpc_error)?;
        self.io.flush().await.map_err(rpc_error)?;
        if let Some(trace) = &mut self.setup_trace {
            trace.record("call.realtime_start_sent", None, json!({}))?;
        }
        self.io.wait_reply(&request).await.map_err(rpc_error)?;
        let answer = self.answer().await?;
        if let Some(trace) = &mut self.setup_trace {
            trace.record("call.answer_received", None, json!({}))?;
        }
        Ok(answer)
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
            self.defer_event(message)?;
        }
    }

    fn defer_event(&mut self, message: Message) -> Result<(), Error> {
        match self.project(message)? {
            Event::Closed => Err(Error::Unavailable),
            Event::Other => Ok(()),
            _ if self.early.len() == 32 => Err(Error::Unavailable),
            event => {
                self.early.push_back(event);
                Ok(())
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
        if let Some((id, deadline, best_effort)) = self.interrupt.clone() {
            let result = timeout_at(deadline, self.io.wait_reply(&id))
                .await
                .map_err(|_| Error::Deadline)
                .and_then(|reply| match reply {
                    // The turn ended before the interrupt reached it.
                    Ok(_) | Err(RpcError::Remote(_)) => Ok(()),
                    Err(error) => Err(rpc_error(error)),
                });
            match result {
                // The call outlives an applied end's unacknowledged interrupt.
                Err(Error::Deadline) if best_effort => {
                    self.io.cancel(&id);
                }
                Err(error) => {
                    self.fail();
                    return Err(error);
                }
                Ok(()) => {
                    if let (Some(trace), Some(turn)) = (&mut self.tool_trace, &self.interrupted) {
                        trace.interrupt("turn.interrupt_acknowledged", turn)?;
                    }
                }
            }
            self.interrupt = None;
        }
        if let Some(event) = self.early.pop_front() {
            return Ok(event);
        }
        let result = match self.io.next_event().await {
            Ok(message) => self.project(message),
            Err(error) => {
                if let Some(trace) = &mut self.tool_trace {
                    let reason = if error == RpcError::Malformed {
                        "control_failed"
                    } else {
                        "app_server_exit"
                    };
                    let _ = trace.close("failed", Some(reason));
                }
                Err(rpc_error(error))
            }
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
        self.io.respond(id, result).map_err(rpc_error)?;
        if let Some(trace) = &mut self.tool_trace {
            trace.answer(id, result)?;
        }
        Ok(())
    }

    /// The negotiated thread belongs to this process generation only.
    #[must_use]
    pub fn thread_id(&self) -> Option<&str> {
        self.thread.as_deref()
    }

    /// Queue one correlated interrupt; `next` drives its acknowledgement while
    /// the media owner continues polling PCM. Cancellation retains the request.
    /// # Errors
    /// Inactive/foreign/empty turns, concurrent interrupts or output admission.
    pub fn interrupt(&mut self, params: &TurnInterrupt, deadline: Instant) -> Result<(), Error> {
        if self.phase != Phase::Active
            || self.thread.as_deref() != Some(params.thread_id.as_str())
            || params.turn_id.is_empty()
            || params.turn_id.len() > 256
            || params.turn_id.chars().any(char::is_control)
            || self.interrupt.is_some()
        {
            return Err(Error::Input);
        }
        if Instant::now() >= deadline {
            return Err(Error::Deadline);
        }
        let id = self.io.begin_request("turn/interrupt", params).map_err(rpc_error)?;
        self.interrupt = Some((id, deadline, false));
        self.interrupted = Some(TurnInterrupt {
            thread_id: params.thread_id.clone(),
            turn_id: params.turn_id.clone(),
        });
        if let Some(trace) = &mut self.tool_trace {
            trace.interrupt("turn/interrupt", params)?;
        }
        Ok(())
    }

    /// Interrupt the turn an applied end leaves running. Best effort: a missed
    /// acknowledgement cancels the request instead of closing the call.
    /// # Errors
    /// As `interrupt`.
    pub fn interrupt_after_end(&mut self, params: &TurnInterrupt, deadline: Instant) -> Result<(), Error> {
        self.interrupt(params, deadline)?;
        if let Some((.., best_effort)) = &mut self.interrupt {
            *best_effort = true;
        }
        Ok(())
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
        if let Some((id, ..)) = self.interrupt.take() {
            self.io.cancel(&id);
        }
        if result.is_err() {
            self.fail();
        } else {
            self.phase = Phase::Closed;
        }
        result
    }

    fn fail(&mut self) {
        if let Some(trace) = &mut self.tool_trace {
            let _ = trace.close("failed", Some("control_failed"));
        }
        self.io.close(RpcError::Disconnected);
        self.phase = Phase::Closed;
        self.early.clear();
        self.interrupt = None;
        self.interrupted = None;
    }
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Drop for Call<'_, R, W> {
    fn drop(&mut self) {
        if self.phase != Phase::Closed && self.phase != Phase::Fresh {
            self.io.close(RpcError::Disconnected);
        }
    }
}

pub(crate) fn rpc_error(error: RpcError) -> Error {
    match error {
        RpcError::Deadline => Error::Deadline,
        RpcError::Malformed => Error::Decode,
        _ => Error::Unavailable,
    }
}

/// The chatgpt call route was measured to drop session `instructions` on v3
/// calls, so the prompt also opens the Live history as a developer item. The
/// pin refuses an item past 8192 estimated tokens (four bytes each); a longer
/// prompt is sent as `prompt` only.
fn live_history(prompt: &str) -> Option<serde_json::Value> {
    (!prompt.is_empty() && prompt.len() <= 32_768).then(|| json!([{"role": "developer", "text": prompt}]))
}
