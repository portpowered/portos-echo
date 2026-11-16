//! The active call owner alone delivers prepared commentary. Workers own no Live socket.
use super::bridge_budget::{Budget, Credentials};
use super::journey::bridge_live::LivePort;
use crate::{
    Error,
    bridge_input::{Provider, Settings},
    bridge_trace::Trace,
};
use echo_config::{McpAuth, McpServer};
use echo_contracts::{
    live::{DelegationTarget, LiveEvent},
    secret::Secret,
};
use echo_delegation::{DelegationRunner, TraceEvent, TraceSink};
use echo_mcp_client::{McpClient, TokenError, TokenProvider};
use serde_json::json;
use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinHandle};

struct Token(McpServer);
impl TokenProvider for Token {
    fn token(&self) -> impl Future<Output = Result<Secret<String>, TokenError>> + Send {
        std::future::ready(self.acquire())
    }
}
impl Token {
    fn acquire(&self) -> Result<Secret<String>, TokenError> {
        match self.0.auth {
            // The existing MCP client always sends a Bearer header. An empty
            // bearer carries no authority; unauthenticated spoof servers ignore it.
            McpAuth::None => Ok(Secret::new(String::new())),
            McpAuth::BearerFile => {
                let path = self.0.bearer_file.as_ref().ok_or(TokenError::Unavailable)?;
                let file = std::fs::File::open(path).map_err(|_| TokenError::Unavailable)?;
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut std::io::Read::take(file, 16_385), &mut bytes)
                    .map_err(|_| TokenError::Unavailable)?;
                let text = String::from_utf8(bytes).map_err(|_| TokenError::Unavailable)?;
                let key = text
                    .strip_suffix("\r\n")
                    .or_else(|| text.strip_suffix('\n'))
                    .unwrap_or(&text);
                if key.len() > 16_384 || key.trim().is_empty() || key.chars().any(char::is_control) {
                    return Err(TokenError::Unavailable);
                }
                Ok(Secret::new(key.into()))
            }
            McpAuth::PortosDeviceCode => Err(TokenError::Unavailable),
        }
    }
}

struct Sink {
    send: mpsc::Sender<TraceEvent>,
    overflow: AtomicBool,
}
impl TraceSink for Sink {
    fn record(&self, event: TraceEvent) {
        // This event observes preparation only, never delivery to Live.
        if !matches!(event, TraceEvent::CommentaryAppend { .. }) && self.send.try_send(event).is_err() {
            self.overflow.store(true, Ordering::Relaxed);
        }
    }
}

pub(super) const MAX_DELEGATIONS: u32 = 1024;

type Runner = DelegationRunner<Token, Credentials, Sink>;
type Completion = (Runner, Result<Option<LiveEvent>, echo_delegation::DelegationError>);

pub(super) struct Host {
    config: Option<(echo_config::AgentConfig, McpServer, echo_signin::Providers)>,
    model_url: Option<String>,
    budget: Arc<Budget>,
    utc_day: Arc<dyn Fn() -> u64 + Send + Sync>,
    credentials: Option<Arc<Credentials>>,
    runner: Option<Runner>,
    pending: Option<JoinHandle<Completion>>,
    sink: Arc<Sink>,
    receive: mpsc::Receiver<TraceEvent>,
    seen: HashSet<String>,
    active: Option<(String, String)>,
}

impl Host {
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
            config,
            model_url: settings.model_url.clone(),
            budget: Arc::new(Budget::new(cap)),
            utc_day,
            credentials: None,
            runner: None,
            pending: None,
            sink: Arc::new(Sink {
                send,
                overflow: AtomicBool::new(false),
            }),
            receive,
            seen: HashSet::new(),
            active: None,
        }
    }

    pub fn new_call(&mut self) {
        self.abandon();
        self.seen.clear();
    }

    fn build(&mut self) -> Result<Runner, Error> {
        // Give replacements their own sink: even a cancellation racing a
        // synchronous tool observation cannot attribute an old edge to a new run.
        let (send, receive) = mpsc::channel(32);
        self.sink = Arc::new(Sink {
            send,
            overflow: AtomicBool::new(false),
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
            Token(server.clone()),
            Duration::from_secs(config.task_deadline_seconds),
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

    pub fn dispatch(&mut self, event: LiveEvent, run: &str, trace: &mut Trace) -> Result<bool, Error> {
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
        let mut runner = match self.runner.take() {
            Some(runner) => runner,
            None => self.build()?,
        };
        if let Some(credentials) = &self.credentials {
            credentials.limited.store(false, Ordering::Relaxed);
            credentials.authorization_failed.store(false, Ordering::Relaxed);
        }
        self.active = Some((run.into(), id));
        self.pending = Some(tokio::spawn(async move {
            let output = runner
                .run(
                    event,
                    "Execute the delegated request using only the configured tools.".into(),
                )
                .await;
            (runner, output)
        }));
        Ok(true)
    }

    pub async fn service(&mut self, call: &mut impl LivePort, trace: &mut Trace) -> Result<(), Error> {
        self.flush(trace)?;
        if !self.pending.as_ref().is_some_and(JoinHandle::is_finished) {
            return Ok(());
        }
        let pending = self.pending.take().ok_or(Error::Unavailable)?;
        let (runner, result) = pending.await.map_err(|_| Error::Unavailable)?;
        self.runner = Some(runner);
        self.flush(trace)?;
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
        call.append_commentary(delegation_id, text).await?;
        trace.record("session.commentary.append", Some(&run), json!({"delegation_id":id}))
    }

    fn flush(&mut self, trace: &mut Trace) -> Result<(), Error> {
        if self.sink.overflow.load(Ordering::Relaxed) {
            return Err(Error::Trace);
        }
        while let Ok(event) = self.receive.try_recv() {
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

    /// Aborting the owner drops the runner and its backend workers; no late result is reusable.
    pub fn abandon(&mut self) -> bool {
        let pending = self.pending.take();
        let abandoned = pending.is_some();
        if let Some(task) = pending {
            task.abort();
        }
        // A retained completed runner keeps its backend accounting. A cancelled
        // runner is rebuilt with the host's still-retained request reservations.
        self.active = None;
        while self.receive.try_recv().is_ok() {}
        abandoned
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.abandon();
    }
}
