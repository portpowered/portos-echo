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
                if matches!(method.as_str(), "turn/started" | "turn/completed") {
                    let turn = params["turn"]["id"]
                        .as_str()
                        .filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
                        .ok_or(Error::Decode)?
                        .to_owned();
                    let thread = self.thread.clone().ok_or(Error::Decode)?;
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
