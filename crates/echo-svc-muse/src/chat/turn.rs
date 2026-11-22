use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::wire::InvalidWire;

const MAX_LINE: usize = 256 * 1024;
const MAX_TEXT: usize = 64 * 1024;
const MAX_IDS: usize = 64;

pub(crate) struct Turn {
    session: String,
    users: Vec<String>,
    pending: Vec<u8>,
    replies: HashMap<String, Reply>,
    ignored: HashSet<String>,
    running: HashSet<String>,
    sequence: [Option<u64>; 2],
    terminal: bool,
    busy: bool,
}

#[derive(Default)]
struct Reply {
    text: String,
    done: bool,
}

impl Turn {
    pub(super) fn new(session: String, users: Vec<String>) -> Self {
        Self {
            session,
            users,
            pending: Vec::new(),
            replies: HashMap::new(),
            ignored: HashSet::new(),
            running: HashSet::new(),
            sequence: [None; 2],
            terminal: false,
            busy: false,
        }
    }

    pub(crate) fn session(&self) -> &str {
        &self.session
    }

    pub(crate) fn next(&mut self, data: &mut &[u8], end: bool) -> Result<Option<Value>, InvalidWire> {
        let newline = data.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(data.len(), |offset| offset + 1);
        if self.pending.len() + count > MAX_LINE {
            return Err(InvalidWire);
        }
        let (part, rest) = data.split_at(count);
        self.pending.extend_from_slice(part);
        *data = rest;
        if newline.is_none() && !end || self.pending.is_empty() {
            return Ok(None);
        }
        let bytes = std::mem::take(&mut self.pending);
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Some(Value::Null));
        }
        serde_json::from_slice(&bytes).map(Some).map_err(|_| InvalidWire)
    }

    pub(crate) fn progress(&mut self, event: &Value) -> Result<Option<String>, InvalidWire> {
        let payload = &event["payload"];
        let session = payload["session_id"].as_str().or_else(|| event["session_id"].as_str());
        if event["type"] != "event" || session != Some(self.session.as_str()) {
            return Ok(None);
        }
        let name = event["event"].as_str().unwrap_or_default();
        let parent = identifier(&payload["reply_to_message_id"]).or_else(|| identifier(&payload["parent_message_id"]));
        let linked = parent.is_some_and(|id| self.is_parent(id));
        let lane = usize::from(name == "message.assistant");
        let sequence = event["seq"].as_u64();
        if sequence.zip(self.sequence[lane]).is_some_and(|(seq, last)| seq <= last) {
            return Ok(None);
        }
        let result = match name {
            "delta.message_start" | "delta.text_append" | "delta.message_done" | "message.assistant" => {
                self.message(event, payload, name, parent.is_none() || linked)?
            }
            "agent.status" | "task.status" if parent.is_none() || linked => {
                self.status(name, payload, linked)?;
                None
            }
            _ => return Ok(None),
        };
        self.sequence[lane] = sequence.or(self.sequence[lane]);
        Ok(result)
    }

    fn is_parent(&self, id: &str) -> bool {
        self.users.iter().any(|user| user == id) || self.replies.contains_key(id)
    }

    fn message(
        &mut self,
        event: &Value,
        payload: &Value,
        name: &str,
        scoped: bool,
    ) -> Result<Option<String>, InvalidWire> {
        let Some(id) = identifier(&payload["message_id"])
            .or_else(|| identifier(&event["message_id"]))
            .or_else(|| identifier(&payload["id"]))
        else {
            return Ok(None);
        };
        if self.users.iter().any(|user| user == id) || self.ignored.contains(id) {
            return Ok(None);
        }
        if !scoped || payload["role"].as_str().is_some_and(|role| role != "assistant") {
            self.ignored.insert(id.into());
            return if self.ignored.len() > MAX_IDS {
                Err(InvalidWire)
            } else {
                Ok(None)
            };
        }
        if !self.replies.contains_key(id) && self.replies.len() >= MAX_IDS {
            return Err(InvalidWire);
        }
        let reply = self.replies.entry(id.to_owned()).or_default();
        reply.update(name, id, payload)
    }

    fn status(&mut self, name: &str, payload: &Value, linked: bool) -> Result<(), InvalidWire> {
        let status = payload["status"].as_str().unwrap_or_default();
        let terminal = matches!(status, "completed" | "failed" | "errored" | "cancelled" | "canceled");
        if name == "agent.status" {
            let activity = payload["activity_code"].as_str().unwrap_or(status);
            self.busy = !terminal && !matches!(activity, "" | "idle" | "online");
        } else if let Some(id) = identifier(&payload["task_id"]) {
            if status == "running" {
                self.running.insert(id.into());
                self.terminal = false;
            } else if terminal && (self.running.remove(id) || linked) {
                self.terminal = true;
            }
        }
        if self.running.len() > MAX_IDS {
            Err(InvalidWire)
        } else {
            Ok(())
        }
    }

    pub(crate) fn finished(&self) -> bool {
        self.terminal
            && !self.busy
            && self.running.is_empty()
            && !self.replies.is_empty()
            && self.replies.values().all(|reply| reply.done)
    }
}

impl Reply {
    fn update(&mut self, name: &str, id: &str, payload: &Value) -> Result<Option<String>, InvalidWire> {
        if self.done || name == "delta.message_start" {
            return Ok(None);
        }
        let text = if name == "delta.text_append" {
            let text = clean(payload["text"].as_str().unwrap_or_default());
            self.text.push_str(&text);
            text
        } else if name == "message.assistant" && payload["display_text_ready"] == false {
            return Ok(None);
        } else {
            self.done = true;
            let text = completed_text(id, payload);
            text.strip_prefix(&self.text).unwrap_or(&text).to_owned()
        };
        if self.text.len() > MAX_TEXT || text.len() > MAX_TEXT {
            return Err(InvalidWire);
        }
        Ok((!text.is_empty()).then_some(text))
    }
}

fn identifier(value: &Value) -> Option<&str> {
    value.as_str().filter(|id| !id.is_empty() && id.len() <= 512)
}

fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
        .collect()
}

fn completed_text(id: &str, payload: &Value) -> String {
    let transcript = &payload["transcript"];
    if let Some(text) = payload["display_text"].as_str().or_else(|| transcript.as_str()) {
        return clean(text);
    }
    let parts = transcript["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .find(|message| message["role"] == "assistant" && message["id"] == id)
        .and_then(|message| message["content"].as_array());
    match parts {
        Some(parts) => parts
            .iter()
            .filter(|item| item["type"] == "text")
            .filter_map(|item| item["text"].as_str())
            .map(clean)
            .collect::<Vec<_>>()
            .join("\n"),
        None => clean(payload["content"].as_str().unwrap_or_default()),
    }
}
