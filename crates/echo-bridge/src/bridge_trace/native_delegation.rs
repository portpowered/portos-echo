//! Call-local observation only; final agent text never proves a native append.
use super::Trace;
use crate::Error;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use tokio::time::Instant;

struct Delegation {
    run: Option<String>,
    thread: String,
    turn: Option<String>,
    started: Instant,
    answer: Option<String>,
    timed_out: bool,
    tools: super::native_tools::Tools,
}

pub(super) struct Native {
    trace: Trace,
    seen: HashSet<String>,
    active: HashMap<String, Delegation>,
    turn: Option<String>,
    retired: HashSet<String>,
}

impl Native {
    pub fn new(trace: Trace) -> Self {
        Self {
            trace,
            seen: HashSet::new(),
            active: HashMap::new(),
            turn: None,
            retired: HashSet::new(),
        }
    }

    pub fn notification(&mut self, method: &str, params: &Value) -> Result<(), Error> {
        match method {
            "thread/realtime/itemAdded" if params["item"]["type"] == "handoff_request" => self.created(params),
            "turn/started" => self.started(params),
            "turn/completed" => self.completed(params),
            "item/completed" if params["item"]["type"] == "agentMessage" => {
                self.answer(params);
                Ok(())
            }
            "thread/realtime/error" => self.close("failed", Some("realtime_error")),
            "thread/realtime/closed" => self.close("cancelled", None),
            _ => Ok(()),
        }
    }

    fn created(&mut self, params: &Value) -> Result<(), Error> {
        let id = identifier(&params["item"]["handoff_id"])?;
        let thread = identifier(&params["threadId"])?;
        if self.seen.contains(id) {
            return Ok(());
        }
        if self.seen.len() >= 1024 {
            return Err(Error::Trace);
        }
        let run = self.trace.active_run.lock().map_err(|_| Error::Trace)?.clone();
        let mut fields = json!({"delegation_id":id,"thread_id":thread,"task":params["item"]});
        if let Some(turn) = &self.turn {
            fields["turn_id"] = turn.clone().into();
        }
        self.trace.record("delegation.created", run.as_deref(), fields)?;
        self.seen.insert(id.into());
        self.active.insert(
            id.into(),
            Delegation {
                run,
                thread: thread.into(),
                turn: self.turn.clone(),
                started: Instant::now(),
                answer: None,
                timed_out: false,
                tools: super::native_tools::Tools::default(),
            },
        );
        Ok(())
    }

    fn started(&mut self, params: &Value) -> Result<(), Error> {
        let turn = identifier(&params["turn"]["id"])?;
        if self.retired.contains(turn) {
            return Ok(());
        }
        self.turn = Some(turn.into());
        for delegation in self.active.values_mut().filter(|entry| entry.turn.is_none()) {
            delegation.turn = Some(turn.into());
        }
        Ok(())
    }

    fn answer(&mut self, params: &Value) {
        // Commentary agent items and voice transcripts are not final answers.
        if params["item"]["phase"]
            .as_str()
            .is_some_and(|phase| phase != "final_answer")
        {
            return;
        }
        let Some(turn) = params["turnId"].as_str() else { return };
        let Some(text) = params["item"]["text"].as_str() else {
            return;
        };
        for delegation in self
            .active
            .values_mut()
            .filter(|entry| entry.turn.as_deref() == Some(turn))
        {
            delegation.answer = Some(text.into());
        }
    }

    fn completed(&mut self, params: &Value) -> Result<(), Error> {
        let turn = identifier(&params["turn"]["id"])?;
        let (outcome, reason) = match params["turn"]["status"].as_str() {
            Some("completed") => ("completed", None),
            Some("failed") => ("failed", Some("turn_failed")),
            Some("interrupted") => ("interrupted", None),
            _ => return Ok(()),
        };
        let ids: Vec<_> = self
            .active
            .iter()
            .filter(|(_, entry)| entry.turn.as_deref() == Some(turn))
            .map(|(id, _)| id.clone())
            .collect();
        if self.turn.as_deref() == Some(turn) {
            self.turn = None;
        }
        if self.retired.len() >= 1024 && !self.retired.contains(turn) {
            return Err(Error::Trace);
        }
        self.retired.insert(turn.into());
        for id in ids {
            self.finish(&id, outcome, reason)?;
        }
        Ok(())
    }

    pub fn timeout(&mut self, turn: &str) {
        for entry in self
            .active
            .values_mut()
            .filter(|entry| entry.turn.as_deref() == Some(turn))
        {
            entry.timed_out = true;
        }
    }

    fn finish(&mut self, id: &str, outcome: &str, reason: Option<&str>) -> Result<(), Error> {
        let Some(entry) = self.active.remove(id) else {
            return Ok(());
        };
        let mut fields = json!({"delegation_id":id,"thread_id":entry.thread,
            "outcome":if entry.timed_out { "timed_out" } else { outcome },
            "handback":"unobserved", "tool_counts":entry.tools.counts,
            "duration_ms":u64::try_from(entry.started.elapsed().as_millis()).unwrap_or(u64::MAX)});
        if let Some(turn) = entry.turn {
            fields["turn_id"] = turn.into();
        }
        if !entry.timed_out
            && let Some(reason) = reason
        {
            fields["reason_class"] = reason.into();
        }
        if let Some(answer) = entry.answer {
            fields["answer"] = json!({"text":answer});
        }
        self.trace.record("delegation.resolved", entry.run.as_deref(), fields)
    }

    pub fn close(&mut self, outcome: &str, reason: Option<&str>) -> Result<(), Error> {
        let ids: Vec<_> = self.active.keys().cloned().collect();
        let mut result = Ok(());
        for id in ids {
            result = result.and(self.finish(&id, outcome, reason));
        }
        self.turn = None;
        result
    }

    pub fn tool(&mut self, method: &str, params: &Value, names: &[(String, String)]) -> Result<(), Error> {
        for (id, entry) in &mut self.active {
            if params["threadId"] != entry.thread || params["turnId"].as_str() != entry.turn.as_deref() {
                continue;
            }
            if let Some(mut fields) = entry.tools.observe(method, &params["item"], names)? {
                fields["delegation_id"] = id.clone().into();
                fields["thread_id"] = entry.thread.clone().into();
                fields["turn_id"] = params["turnId"].clone();
                self.trace.record("delegation.tool", entry.run.as_deref(), fields)?;
            }
        }
        Ok(())
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        let _ = self.close("cancelled", None);
    }
}

fn identifier(value: &Value) -> Result<&str, Error> {
    value
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
        .ok_or(Error::Decode)
}
