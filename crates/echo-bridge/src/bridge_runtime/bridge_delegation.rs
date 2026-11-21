//! The active call owner alone delivers prepared commentary. Workers own no Live socket.
mod bridge_client_trace;
mod bridge_host;
mod bridge_muse_host;
mod bridge_muse_work;
mod bridge_resolution;
use super::bridge_budget::{Budget, Credentials};
use super::journey::bridge_live::LivePort;
use crate::bridge_mcp::{Grant, Token};
use crate::{
    Error,
    bridge_input::{Provider, Settings},
    bridge_trace::Trace,
};
pub(super) use bridge_host::Host;
use echo_config::McpServer;
use echo_contracts::live::{DelegationTarget, LiveEvent};
use echo_delegation::{DelegationRunner, TraceEvent, TraceSink};
use echo_mcp_client::McpClient;
use serde_json::json;
use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::Instant,
};

enum Observation {
    Trace(TraceEvent),
    Emotion(echo_screen_contract::UiState, tokio::time::Instant),
}

struct Sink {
    send: mpsc::Sender<Observation>,
    overflow: AtomicBool,
    trace: bridge_client_trace::ClientTrace,
}
impl TraceSink for Sink {
    fn record(&self, event: TraceEvent) {
        // This event observes preparation only, never delivery to Live.
        if !matches!(
            event,
            TraceEvent::CommentaryAppend { .. } | TraceEvent::DelegationCreated { .. }
        ) && self.send.try_send(Observation::Trace(event)).is_err()
        {
            self.overflow.store(true, Ordering::Relaxed);
        }
    }
    fn tool_result(&self, name: &str, content: &serde_json::Value, is_error: bool) {
        if let Some((state, deadline)) = super::bridge_screen::emotion::decode(name, content, is_error)
            && self.send.try_send(Observation::Emotion(state, deadline)).is_err()
        {
            self.overflow.store(true, Ordering::Relaxed);
        }
    }
    fn task_terminal(&self, event: &echo_contracts::task::TaskEvent) {
        self.trace.terminal(event);
    }
    fn tool_completed(
        &self,
        name: &str,
        arguments: &serde_json::Value,
        result: &Result<echo_agent_api::ToolResult, echo_agent_api::ToolError>,
        duration: Duration,
    ) {
        self.trace.tool(name, arguments, result, duration);
    }
}

pub(super) const MAX_DELEGATIONS: u32 = 1024;
const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

type Runner = DelegationRunner<Token, Credentials, Sink>;
type Completion = (Runner, Result<Option<LiveEvent>, echo_delegation::DelegationError>);

pub(in crate::bridge_runtime) struct Client {
    grant: Option<Arc<Grant>>,
    config: Option<(echo_config::AgentConfig, McpServer, echo_signin::Providers)>,
    model_url: Option<String>,
    budget: Arc<Budget>,
    utc_day: Arc<dyn Fn() -> u64 + Send + Sync>,
    credentials: Option<Arc<Credentials>>,
    runner: Option<Runner>,
    stop: Option<oneshot::Sender<()>>,
    pending: Option<JoinHandle<Completion>>,
    completed: Option<Result<Completion, tokio::task::JoinError>>,
    sink: Arc<Sink>,
    receive: mpsc::Receiver<Observation>,
    observed: Option<Observation>,
    seen: HashSet<String>,
    active: Option<(String, String)>,
}

impl Client {
    pub fn new(settings: &Settings, utc_day: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        let config = match (&settings.delegation, &settings.credential) {
            (Some((agent, server)), Provider::Resolved(providers)) => {
                Some((agent.clone(), server.clone(), providers.clone()))
            }
            _ => None,
        };
        let cap = config.as_ref().map_or(0, |(agent, _, _)| agent.daily_request_cap);
        let (send, receive) = mpsc::channel(32);
        Self {
            grant: None,
            config,
            model_url: settings.model_url.clone(),
            budget: Arc::new(Budget::new(cap)),
            utc_day,
            credentials: None,
            runner: None,
            stop: None,
            pending: None,
            completed: None,
            sink: Arc::new(Sink {
                send,
                overflow: AtomicBool::new(false),
                trace: bridge_client_trace::ClientTrace::default(),
            }),
            receive,
            observed: None,
            seen: HashSet::new(),
            active: None,
        }
    }

    pub async fn new_call(&mut self) {
        self.abandon().await;
        self.seen.clear();
    }

    pub fn grant(&mut self, grant: Arc<Grant>) {
        self.grant = Some(grant);
    }

    fn build(&mut self) -> Result<Runner, Error> {
        // Give replacements their own sink: even a cancellation racing a
        // synchronous tool observation cannot attribute an old edge to a new run.
        let (send, receive) = mpsc::channel(32);
        self.sink = Arc::new(Sink {
            send,
            overflow: AtomicBool::new(false),
            trace: bridge_client_trace::ClientTrace::default(),
        });
        self.receive = receive;
        let (config, server, providers) = self.config.as_ref().ok_or(Error::Input)?;
        let credentials = Arc::new(Credentials {
            providers: providers.clone(),
            budget: self.budget.clone(),
            limited: AtomicBool::new(false),
            authorization_failed: AtomicBool::new(false),
            utc_day: self.utc_day.clone(),
        });
        let host = McpClient::new(
            server.clone(),
            Token::new(server.clone(), self.grant.clone()),
            MCP_REQUEST_TIMEOUT,
        )
        .map_err(|_| Error::Input)?;
        let runner = Runner::new(
            config.clone(),
            providers.mode(),
            Arc::new(host),
            credentials.clone(),
            self.sink.clone(),
            self.model_url.as_deref(),
        )
        .map_err(|_| Error::Input)?;
        self.credentials = Some(credentials);
        Ok(runner)
    }

    pub fn dispatch(&mut self, event: LiveEvent, run: &str, trace: &mut Trace, cutoff: Instant) -> Result<bool, Error> {
        if let Some(grant) = &self.grant {
            grant.check()?;
            if grant.blocked() {
                trace.record("delegation.refused", Some(run), json!({"reason":"mcp_relink_required"}))?;
                return Ok(false);
            }
        }
        let LiveEvent::DelegationCreated { delegation, .. } = &event else {
            return Ok(false);
        };
        let id = delegation.id.to_string();
        if id.len() > 256 || id.chars().any(char::is_control) {
            return Err(Error::Decode);
        }
        if self.seen.contains(&id) {
            return Ok(false);
        }
        if self.seen.len() >= MAX_DELEGATIONS as usize {
            return Err(Error::Unavailable);
        }
        self.seen.insert(id.clone());
        if delegation.target != DelegationTarget::Client || self.pending.is_some() {
            trace.record(
                "delegation.refused",
                Some(run),
                json!({"delegation_id":id,"reason":"target_or_busy"}),
            )?;
            return Ok(false);
        }
        let runner = match self.runner.take() {
            Some(runner) => runner,
            None => self.build()?,
        };
        if let Some(credentials) = &self.credentials {
            credentials.limited.store(false, Ordering::Relaxed);
            credentials.authorization_failed.store(false, Ordering::Relaxed);
        }
        let server = &self.config.as_ref().ok_or(Error::Input)?.1.name;
        self.sink.trace.begin(
            trace,
            run,
            &id,
            &serde_json::to_value(&event).map_err(|_| Error::Decode)?,
            server,
        )?;
        self.active = Some((run.into(), id));
        self.start(runner, event, cutoff);
        Ok(true)
    }

    fn start(&mut self, mut runner: Runner, event: LiveEvent, cutoff: Instant) {
        let (stop, receive) = oneshot::channel();
        self.stop = Some(stop);
        self.pending = Some(tokio::spawn(async move {
            let output = runner
                .run_until_cancelled(
                    event,
                    "Execute the delegated request using only the configured tools.".into(),
                    async {
                        if Instant::now() < cutoff {
                            let _ = receive.await;
                        }
                    },
                )
                .await;
            (runner, output)
        }));
    }

    /// Await worker completion without requiring another Live, device or timer event.
    /// Retain the result when select cancels the other owner futures.
    pub async fn ready(&mut self) {
        let Some(pending) = self.pending.as_mut() else {
            return std::future::pending().await;
        };
        tokio::select! {
            completed = pending => {
                self.completed = Some(completed);
                self.pending = None;
            },
            event = self.receive.recv() => self.observed = event,
        }
    }

    pub async fn service(
        &mut self,
        call: &mut impl LivePort,
        trace: &mut Trace,
        screen: &super::bridge_screen::Screen,
    ) -> Result<(), Error> {
        self.flush(trace, screen)?;
        if self.pending.as_ref().is_some_and(JoinHandle::is_finished) {
            self.ready().await;
        }
        let Some(completed) = self.completed.take() else {
            return Ok(());
        };
        self.stop = None;
        let result = self.complete(completed, call, trace, screen).await;
        if result.is_err() {
            let _ = self.sink.trace.finish(Some("worker_failed"), "unobserved", None);
        }
        result
    }

    async fn complete(
        &mut self,
        completed: Result<Completion, tokio::task::JoinError>,
        call: &mut impl LivePort,
        trace: &mut Trace,
        screen: &super::bridge_screen::Screen,
    ) -> Result<(), Error> {
        let (runner, result) = completed.map_err(|_| Error::Unavailable)?;
        self.runner = Some(runner);
        self.flush(trace, screen)?;
        if let Some(grant) = &self.grant {
            grant.check()?;
            if grant.blocked() {
                return Err(Error::Mcp(echo_mcp_auth::AuthError::RelinkRequired));
            }
        }
        if self
            .credentials
            .as_ref()
            .is_some_and(|c| c.authorization_failed.load(Ordering::Relaxed))
        {
            return Err(Error::Authorization);
        }
        let (run, id) = self.active.take().ok_or(Error::Unavailable)?;
        let Some(LiveEvent::CommentaryAppend {
            delegation_id,
            mut text,
            ..
        }) = result.map_err(|_| Error::Unavailable)?
        else {
            return Err(Error::Unavailable);
        };
        if self
            .credentials
            .as_ref()
            .is_some_and(|c| c.limited.load(Ordering::Relaxed))
        {
            text = "daily request limit reached".into();
        }
        if let Err(error) = call.append_commentary(delegation_id, text.clone()).await {
            let _ = self.sink.trace.finish(Some("append_failed"), "failed", Some(&text));
            return Err(error.into());
        }
        let receipt = trace.record("session.commentary.append", Some(&run), json!({"delegation_id":id}));
        let terminal = self.sink.trace.finish(None, "submitted", Some(&text));
        receipt?;
        terminal
    }

    fn flush(&mut self, trace: &mut Trace, screen: &super::bridge_screen::Screen) -> Result<(), Error> {
        self.sink.trace.check()?;
        if self.sink.overflow.load(Ordering::Relaxed) {
            return Err(Error::Trace);
        }
        while let Some(event) = self.observed.take().or_else(|| self.receive.try_recv().ok()) {
            let event = match event {
                Observation::Trace(event) => event,
                Observation::Emotion(state, deadline) => {
                    if self.active.is_some() && deadline > tokio::time::Instant::now() {
                        screen.presentation.emotion(state, deadline)?;
                    }
                    continue;
                }
            };
            let (run, id) = self.active.as_ref().ok_or(Error::Trace)?;
            let fields = match &event {
                TraceEvent::ToolCall { name } => {
                    let allowed = self
                        .config
                        .as_ref()
                        .is_some_and(|(_, server, _)| server.allow_tools.contains(name));
                    json!({"delegation_id":id,"name":if allowed { name.as_str() } else { "[unlisted]" }})
                }
                _ => json!({"delegation_id":id}),
            };
            trace.record(event.name(), Some(run), fields)?;
        }
        Ok(())
    }

    fn cancel(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }

    /// Signal and join work before another run can use the call.
    pub async fn abandon(&mut self) -> bool {
        self.cancel();
        let abandoned = self.pending.is_some() || self.completed.is_some();
        if let Some(task) = self.pending.as_mut() {
            let _ = task.await;
        }
        self.pending = None;
        self.completed = None;
        self.discard();
        abandoned
    }

    fn discard(&mut self) {
        // The next runner still uses the host's retained request reservations.
        let _ = self.sink.trace.finish(None, "unobserved", None);
        self.active = None;
        self.observed = None;
        while self.receive.try_recv().is_ok() {}
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Some(task) = self.pending.take() {
            task.abort();
        }
        self.discard();
    }
}

#[cfg(test)]
mod tests;
