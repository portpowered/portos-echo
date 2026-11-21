use super::{Call, Event, Phase};
use crate::Error;
use echo_codex::codex_protocol::{KnownNotification, Message};
use tokio::io::{AsyncRead, AsyncWrite};

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Call<'_, R, W> {
    pub(super) fn project(&mut self, message: Message) -> Result<Event, Error> {
        match message {
            Message::Request { id, method, params } => {
                // Unknown or foreign requests are answered without an effect.
                if method == "item/tool/call" && params["threadId"].as_str() == self.thread.as_deref() {
                    let call = serde_json::from_value(params).map_err(|_| Error::Decode)?;
                    if let Some(trace) = &mut self.tool_trace {
                        trace.request(&id, &call)?;
                    }
                    return Ok(Event::Tool { id, call });
                }
                self.io
                    .respond(&id, &serde_json::json!({"contentItems":[],"success":false}))
                    .map_err(super::rpc_error)?;
                Ok(Event::Other)
            }
            Message::Notification { method, params, raw } => {
                if params["threadId"].as_str() != self.thread.as_deref() {
                    return Ok(Event::Other);
                }
                self.trace_notification(&method, &params)?;
                if method == "thread/realtime/itemAdded" {
                    return Ok(self.realtime_item(&params["item"]));
                }
                if method == "item/completed" {
                    self.agent_item(&params);
                    return Ok(Event::Other);
                }
                if matches!(method.as_str(), "turn/started" | "turn/completed") {
                    return self.turn_lifecycle(&method, &params);
                }
                self.notification(&method, raw)
            }
            _ => Ok(Event::Other),
        }
    }

    fn turn_lifecycle(&mut self, method: &str, params: &serde_json::Value) -> Result<Event, Error> {
        let turn = params["turn"]["id"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
            .ok_or(Error::Decode)?
            .to_owned();
        let thread = self.thread.clone().ok_or(Error::Decode)?;
        if method == "turn/completed"
            && self
                .interrupted
                .as_ref()
                .is_some_and(|interrupted| interrupted.thread_id == thread && interrupted.turn_id == turn)
        {
            if params["turn"]["status"] == "interrupted"
                && let Some(trace) = &mut self.tool_trace
                && let Some(interrupted) = &self.interrupted
            {
                trace.interrupt("turn.interrupted", interrupted)?;
            }
            self.interrupted = None;
        }
        Ok(if method == "turn/started" {
            Event::TurnStarted { thread, turn }
        } else {
            let answered = self.answered.as_ref() == Some(&turn);
            if answered {
                self.answered = None;
            }
            Event::TurnCompleted { thread, turn, answered }
        })
    }

    /// Final runner text is handed back to the voice; commentary is not.
    fn agent_item(&mut self, params: &serde_json::Value) {
        let item = &params["item"];
        if item["type"] == "agentMessage"
            && item["phase"].as_str().is_none_or(|phase| phase == "final_answer")
            && item["text"].as_str().is_some_and(|text| !text.trim().is_empty())
            && let Some(turn) = params["turnId"].as_str()
        {
            self.answered = Some(turn.into());
        }
    }

    fn realtime_item(&mut self, item: &serde_json::Value) -> Event {
        match item["type"].as_str() {
            Some("input_audio_buffer.speech_started") => Event::SpeechStarted,
            Some("input_audio_buffer.speech_stopped") => Event::SpeechStopped,
            Some("handoff_request") => {
                let id = item["handoff_id"]
                    .as_str()
                    .filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control));
                if let Some(id) = id
                    && self.handoffs.len() < 1024
                    && self.handoffs.insert(id.into())
                    && let Ok(id) = echo_contracts::ids::DelegationId::new(id)
                {
                    return Event::ReplyHandoff(id);
                }
                Event::Other
            }
            _ => Event::Other,
        }
    }

    pub(super) async fn flush_tool_answer(&mut self) -> Result<(), Error> {
        self.io.flush().await.map_err(super::rpc_error)
    }

    fn trace_notification(&mut self, method: &str, params: &serde_json::Value) -> Result<(), Error> {
        if let Some(trace) = &mut self.tool_trace {
            // Requested stop is Phase::Starting; only an unsolicited active
            // closure is a failure. Projection and shutdown behavior stay intact.
            if method == "thread/realtime/closed" && self.phase == Phase::Active {
                trace.close("failed", Some("realtime_closed"))?;
            }
            trace.notification(method, params)?;
        }
        Ok(())
    }

    fn notification(&mut self, method: &str, raw: serde_json::Value) -> Result<Event, Error> {
        if !matches!(
            method,
            "thread/realtime/transcript/delta"
                | "thread/realtime/transcript/done"
                | "thread/realtime/error"
                | "thread/realtime/closed"
        ) {
            return Ok(Event::Other);
        }
        match serde_json::from_value::<KnownNotification>(raw).map_err(|_| Error::Decode)? {
            KnownNotification::TranscriptDelta(delta) => Ok(Event::Transcript {
                role: delta.role,
                text: delta.delta,
                done: false,
            }),
            KnownNotification::TranscriptDone(done) if done.role == "assistant" => {
                Ok(Event::ReplyDone { text: done.text })
            }
            KnownNotification::TranscriptDone(done) => Ok(Event::Transcript {
                role: done.role,
                text: done.text,
                done: true,
            }),
            KnownNotification::Closed(_) => {
                self.phase = Phase::Closed;
                Ok(Event::Closed)
            }
            KnownNotification::Error(_) => Err(Error::Unavailable),
            _ => Ok(Event::Other),
        }
    }
}
