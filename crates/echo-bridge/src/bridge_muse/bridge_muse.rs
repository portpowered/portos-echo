//! Bridge-local adapter for the existing Muse HTTP `AgentBackend` boundary.
mod bridge_http;
mod bridge_state;

use crate::Error;
use bridge_http::{Http, OPERATION};
use bridge_state::{Session, admission};
use echo_agent_api::{AgentBackend, AgentError, Task};
use echo_config::MuseConfig;
use echo_contracts::{ids::TaskId, task::TaskEvent};
use reqwest::Method;
use std::{collections::HashMap, time::Duration};
use tokio::{sync::Mutex, time::Instant};

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
            let result = tokio::time::timeout_at(
                session.deadline,
                self.http.request(Method::GET, &path(id, "events"), None),
            )
            .await;
            match result {
                Ok(Ok((204, body))) if body.is_empty() => {
                    session.pending = false;
                    // Empty terminal queues can answer immediately. Bound their poll rate.
                    tokio::time::sleep_until((Instant::now() + Duration::from_millis(20)).min(session.deadline)).await;
                }
                Ok(Ok((200, body))) => return session.event(id, &body),
                Ok(_) => return session.fail(id, bridge_state::UNAVAILABLE),
                Err(_) => return session.fail(id, bridge_state::DEADLINE),
            }
        }
    }
}

impl AgentBackend for Backend {
    async fn submit(&self, task: Task) -> Result<TaskId, AgentError> {
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(task.deadline_seconds))
            .ok_or(AgentError::UnscriptedTask)?;
        let body = serde_json::to_vec(&task).map_err(|_| AgentError::UnscriptedTask)?;
        if body.len() > 65536 || deadline <= Instant::now() {
            return Err(AgentError::UnscriptedTask);
        }
        let mut sessions = self.sessions.lock().await;
        if sessions.len() >= 1024 {
            return Err(AgentError::UnscriptedTask);
        }
        let (status, body) = tokio::time::timeout_at(
            deadline.min(Instant::now() + OPERATION),
            self.http.request(Method::POST, "/v1/tasks", Some(body)),
        )
        .await
        .map_err(|_| AgentError::UnscriptedTask)?
        .map_err(|_| AgentError::UnscriptedTask)?;
        let id = admission(status, &body).map_err(|_| AgentError::UnscriptedTask)?;
        if sessions.contains_key(&id) {
            return Err(AgentError::UnscriptedTask);
        }
        sessions.insert(id.clone(), Session::new(deadline));
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
