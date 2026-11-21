//! Bridge-local adapter for the existing Muse HTTP `AgentBackend` boundary.
mod bridge_http;
mod bridge_state;

use crate::Error;
pub(crate) use bridge_http::Http;
use bridge_state::{Session, admission};
use echo_agent_api::{AgentBackend, AgentError, Task};
use echo_config::MuseConfig;
use echo_contracts::{ids::TaskId, task::TaskEvent};
use reqwest::Method;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Mutex;

pub(crate) const TOOL: &str = "ask_muse";

/// Clear withdraws the launch generation's authority even if exec fails.
/// A later save never re-enables that generation.
#[derive(Clone, Default)]
pub(crate) struct Access(Arc<AtomicBool>);

impl Access {
    pub(crate) fn allows(&self) -> bool {
        !self.0.load(Ordering::Acquire)
    }

    pub(crate) fn withdraw(&self) {
        self.0.store(true, Ordering::Release);
    }
}

pub(crate) fn tool_spec() -> echo_agent_api::ToolSpec {
    echo_agent_api::ToolSpec {
        name: TOOL.into(),
        description: "Ask Muse to handle the user’s request and report the result.".into(),
        input_schema: serde_json::json!({
            "type":"object",
            "additionalProperties":false,
            "required":["request"],
            "properties":{"request":{"type":"string","minLength":1}}
        }),
    }
}

pub struct Backend {
    http: Http,
    sessions: Mutex<HashMap<TaskId, Session>>,
}

impl Backend {
    /// Loads only the nominated Muse trust and credential files; never a provider grant.
    /// # Errors
    /// Returns fixed input diagnostics for invalid trust, credentials or origin.
    pub fn open(config: &MuseConfig, local: bool) -> Result<Self, Error> {
        Ok(Self {
            http: Http::open(config, local)?,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    async fn poll(&self, id: &TaskId, session: &mut Session) -> TaskEvent {
        loop {
            if let Some(event) = session.before_poll(id) {
                return event;
            }
            match self.http.request(Method::GET, &path(id, "events"), None).await {
                Ok((204, body)) if body.is_empty() => {
                    session.pending = false;
                    // Empty terminal queues can answer immediately. Bound their poll rate.
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Ok((200, body)) => return session.event(id, &body),
                _ => return session.fail(id, bridge_state::UNAVAILABLE),
            }
        }
    }
}

impl AgentBackend for Backend {
    async fn submit(&self, task: Task) -> Result<TaskId, AgentError> {
        let body = serde_json::to_vec(&task).map_err(|_| AgentError::UnscriptedTask)?;
        if body.len() > 65536 || task.deadline_seconds == 0 {
            return Err(AgentError::UnscriptedTask);
        }
        let mut sessions = self.sessions.lock().await;
        if sessions.len() >= 1024 {
            return Err(AgentError::UnscriptedTask);
        }
        let (status, body) = self
            .http
            .request(Method::POST, "/v1/tasks", Some(body))
            .await
            .map_err(|_| AgentError::UnscriptedTask)?;
        let id = admission(status, &body).map_err(|_| AgentError::UnscriptedTask)?;
        if sessions.contains_key(&id) {
            return Err(AgentError::UnscriptedTask);
        }
        sessions.insert(id.clone(), Session::new());
        Ok(id)
    }

    async fn next_event(&self, id: &TaskId) -> Option<TaskEvent> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions.get_mut(id)?;
        if session.terminal {
            return None;
        }
        Some(self.poll(id, session).await)
    }

    async fn cancel(&self, id: &TaskId) -> Result<(), AgentError> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions.get_mut(id).ok_or(AgentError::UnknownTask)?;
        if session.cancelled {
            return Ok(());
        }
        // Latch before the uncertain network mutation, so a late poll can never revive it.
        session.cancelled = true;
        session.terminal = true;
        match self.http.request(Method::POST, &path(id, "cancel"), None).await {
            Ok((204, body)) if body.is_empty() => Ok(()),
            _ => Err(AgentError::UnscriptedTask),
        }
    }
}

fn path(id: &TaskId, action: &str) -> String {
    // Admission permits only a bounded unreserved path segment.
    format!("/v1/tasks/{id}/{action}")
}
