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
                if matches!(method.as_str(), "turn/started" | "turn/completed") {
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
                    return Ok(if method == "turn/started" {
                        Event::TurnStarted { thread, turn }
                    } else {
                        Event::TurnCompleted { thread, turn }
                    });
                }
                self.notification(&method, raw)
            }
            _ => Ok(Event::Other),
        }
    }

    fn realtime_item(&mut self, item: &serde_json::Value) -> Event {
        match item["type"].as_str() {
            Some("input_audio_buffer.speech_started") => Event::SpeechStarted,
            Some("handoff_request") if item["input_transcript"] == "end the session" => {
                let id = item["handoff_id"]
                    .as_str()
                    .filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control));
                if let Some(id) = id
                    && self.goodbye_handoffs.len() < 1024
                    && self.goodbye_handoffs.insert(id.into())
                {
                    return Event::GoodbyeHandoff;
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
