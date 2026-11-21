//! Correlate supported-route commands before they reach a local owner.
use echo_contracts::endpoint::{CommandHeader, CommandReply, CommandTarget, EndSession, StartSession, Uplink, VERSION};
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::time::Instant;

const RECENT_COMMANDS: usize = 256;

#[derive(Default)]
pub(super) struct Receipts(HashMap<String, Instant>);

pub(super) struct Command {
    header: CommandHeader,
    payload: Value,
    received: Instant,
}

impl Receipts {
    pub(super) fn accept(&mut self, route: &str, text: &str, received: Instant, deadline: Instant) -> Option<Command> {
        let mut received_json: Value = serde_json::from_str(text).ok()?;
        let envelope = received_json.as_object_mut()?;
        let header: CommandHeader = serde_json::from_value(envelope.remove("header")?).ok()?;
        let target: CommandTarget = serde_json::from_value(envelope.remove("target")?).ok()?;
        if target.value != route
            || header.version != VERSION
            || header.message_id.trim().is_empty()
            || header.name.trim().is_empty()
            || Instant::now() >= deadline
        {
            return None;
        }
        self.0.retain(|_, expires| *expires > received);
        if self.0.len() >= RECENT_COMMANDS || self.0.contains_key(&header.message_id) {
            return None;
        }
        self.0.insert(header.message_id.clone(), deadline);
        Some(Command {
            header,
            payload: envelope.remove("payload").unwrap_or_default(),
            received,
        })
    }
}

impl Command {
    pub(super) async fn reply(self, route: &str, lease: tokio::sync::watch::Receiver<()>) -> Option<Vec<u8>> {
        let Self {
            header,
            payload,
            received,
        } = self;
        let outcome = correlate(&header, payload, received, lease).await;
        let reply = CommandReply {
            request_id: &header.message_id,
            outcome,
        };
        serde_json::to_vec(&Uplink::new(&header.name, route, reply)).ok()
    }
}

async fn correlate(
    header: &CommandHeader,
    payload: Value,
    received: Instant,
    lease: tokio::sync::watch::Receiver<()>,
) -> Value {
    match (header.namespace.as_str(), header.name.as_str()) {
        ("assistant-session", "start-session") if payload.is_object() => {
            let Ok(start) = serde_json::from_value::<StartSession>(payload) else {
                return invalid();
            };
            if let Some(voice) = super::voice::Voice::current() {
                voice.start(start, received, lease).await
            } else {
                super::voice::unavailable()
            }
        }
        ("assistant-session", "end-session") if payload.is_object() => {
            let Ok(end) = serde_json::from_value::<EndSession>(payload) else {
                return invalid();
            };
            if end.session_id.trim().is_empty() {
                return invalid();
            }
            if let Some(voice) = super::voice::Voice::current() {
                voice.end(&end.session_id, received).await
            } else {
                super::voice::not_found()
            }
        }
        ("assistant-session", "start-session" | "end-session") => invalid(),
        _ => refusal("UNSUPPORTED_COMMAND", "This command is unsupported."),
    }
}

pub(crate) fn refusal(code: &str, message: &str) -> Value {
    json!({"ok":false,"failure":{"code":code,"message":message}})
}

fn invalid() -> Value {
    refusal("INVALID_REQUEST", "The command payload is invalid.")
}
