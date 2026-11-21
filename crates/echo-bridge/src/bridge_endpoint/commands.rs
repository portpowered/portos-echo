//! Correlate supported-route commands before they reach a local owner.
use echo_contracts::endpoint::{CommandHeader, CommandReply, CommandTarget, EndSession, StartSession, Uplink, VERSION};
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::time::Instant;

const RECENT_COMMANDS: usize = 256;

#[derive(Default)]
pub(super) struct Receipts(HashMap<String, Instant>);

impl Receipts {
    pub(super) fn reply(&mut self, route: &str, text: &str, received: Instant, deadline: Instant) -> Option<Vec<u8>> {
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
        // A valid header and target permit a refusal even without a payload.
        let outcome = correlate(&header, envelope.remove("payload").unwrap_or_default());
        let reply = CommandReply {
            request_id: &header.message_id,
            outcome,
        };
        let body = serde_json::to_vec(&Uplink::new(&header.name, route, reply)).ok()?;
        self.0.insert(header.message_id, deadline);
        Some(body)
    }
}

fn correlate(header: &CommandHeader, payload: Value) -> Value {
    // Until the shared voice owner is attached, a valid start cannot open a
    // session and no end can match a locally owned session.
    match (header.namespace.as_str(), header.name.as_str()) {
        ("assistant-session", "start-session") if payload.is_object() => {
            serde_json::from_value::<StartSession>(payload).map_or_else(
                |_| invalid(),
                |_| refusal("NOT_READY", "The voice provider is unavailable."),
            )
        }
        ("assistant-session", "end-session") if payload.is_object() => serde_json::from_value::<EndSession>(payload)
            .map_or_else(
                |_| invalid(),
                |end| {
                    if end.session_id.trim().is_empty() {
                        return invalid();
                    }
                    refusal("NOT_FOUND", "The voice session was not found.")
                },
            ),
        ("assistant-session", "start-session" | "end-session") => invalid(),
        _ => refusal("UNSUPPORTED_COMMAND", "This command is unsupported."),
    }
}

fn refusal(code: &str, message: &str) -> Value {
    json!({"ok":false,"failure":{"code":code,"message":message}})
}

fn invalid() -> Value {
    refusal("INVALID_REQUEST", "The command payload is invalid.")
}
