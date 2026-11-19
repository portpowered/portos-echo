//! One process-owned MCP/policy/ledger lifetime spans every voice call.
use super::session_tools::{END, EXTEND};
use super::tool_policy::Policy;
use crate::{
    Error,
    bridge_codex::{SessionCommand, SessionRequest, ToolBudget},
    bridge_mcp::{self, Token},
    bridge_notes::{Local, NAMES, Notes},
    bridge_runtime::bridge_screen::Screen,
    bridge_trace::Trace,
};
use echo_agent_api::{ToolError, ToolHost, ToolResult, ToolSpec};
use echo_config::{AgentConfig, BridgeConfig};
use echo_delegation::codex_dispatch::Catalog;
use echo_mcp_client::McpClient;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot};

pub(super) struct Mcp {
    client: Option<McpClient<Token>>,
    trace: Trace,
}
impl ToolHost for Mcp {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>, ToolError> {
        match &self.client {
            Some(host) => host.list_tools().await,
            None => Ok(vec![]),
        }
    }
    async fn call_tool(&self, name: &str, args: Value, key: &str) -> Result<ToolResult, ToolError> {
        let client = self.client.as_ref().ok_or(ToolError::Denied)?;
        self.trace
            .clone()
            .tool_attempt(name, key)
            .map_err(|_| ToolError::Transport)?;
        client.call_tool(name, args, key).await
    }
}

/// Bridge-owned session tools, advertised after the discovered MCP tools.
pub(super) fn session_specs() -> [ToolSpec; 2] {
    [
        ToolSpec {
            name: EXTEND.into(),
            description: "Extend listening for this voice session within its configured bound.".into(),
            input_schema: json!({"type":"object","properties":{"seconds":{"type":"integer","minimum":1}},
                "required":["seconds"],"additionalProperties":false}),
        },
        ToolSpec {
            name: END.into(),
            description: "End this voice session after its current reply has played.".into(),
            input_schema: json!({"type":"object","properties":{},"additionalProperties":false}),
        },
    ]
}

/// Routes session builtins to the run owner as validated commands; every
/// other name keeps the MCP policy host's consent, ledger and authorization.
#[derive(Clone)]
pub(super) struct Session<H> {
    host: H,
    requests: mpsc::Sender<SessionRequest>,
    run: tokio::sync::watch::Receiver<Option<Arc<()>>>,
}

impl<H> Session<H> {
    pub(super) fn new(
        host: H,
        requests: mpsc::Sender<SessionRequest>,
        run: tokio::sync::watch::Receiver<Option<Arc<()>>>,
    ) -> Self {
        Self { host, requests, run }
    }
}

impl<H: ToolHost + Sync> ToolHost for Session<H> {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>, ToolError> {
        self.host.list_tools().await
    }
    async fn call_tool(&self, name: &str, args: Value, key: &str) -> Result<ToolResult, ToolError> {
        // Raw arguments never leave this host; only the parsed command does.
        let command = match name {
            EXTEND => match args.as_object().map(|object| (object.len(), object.get("seconds"))) {
                Some((1, Some(seconds))) => seconds
                    .as_u64()
                    .filter(|seconds| *seconds > 0)
                    .map_or(SessionCommand::Invalid { end: false }, SessionCommand::Extend),
                _ => SessionCommand::Invalid { end: false },
            },
            END if args.as_object().is_some_and(serde_json::Map::is_empty) => SessionCommand::End,
            END => SessionCommand::Invalid { end: true },
            _ => return self.host.call_tool(name, args, key).await,
        };
        let (reply, answer) = oneshot::channel();
        let request = SessionRequest {
            command,
            key: key.into(),
            run: self.run.borrow().clone(),
            reply,
        };
        self.requests.send(request).await.map_err(|_| ToolError::Transport)?;
        answer.await.map_err(|_| ToolError::Transport)
    }
}

pub(super) struct Tools {
    pub host: Option<Local<Policy<Mcp>>>,
    pub config: AgentConfig,
    pub budget: ToolBudget,
    allowed: Vec<String>,
    specs: Option<Vec<ToolSpec>>,
    runtime: Option<bridge_mcp::Runtime>,
    trace: Option<Trace>,
}

impl Tools {
    pub fn new(config: &BridgeConfig, utc_day: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        Self {
            host: None,
            config: config.agent.clone(),
            budget: ToolBudget::new(config.agent.daily_request_cap, move || Ok(utc_day())),
            allowed: config
                .mcp
                .server
                .first()
                .map_or_else(Vec::new, |server| server.allow_tools.clone()),
            specs: None,
            runtime: None,
            trace: None,
        }
    }

    pub async fn open(
        &mut self,
        config: &BridgeConfig,
        screen: &Screen,
        trace: &mut Trace,
        epoch: Option<u64>,
    ) -> Result<(), Error> {
        if let Some(settings) = bridge_mcp::Settings::configured(config)? {
            self.runtime =
                Some(bridge_mcp::Runtime::open(settings, screen.presentation.clone(), trace.clone(), epoch).await?);
            trace.record(
                "mcp.operator.listening",
                None,
                serde_json::json!({
                    "address": self.runtime.as_ref().ok_or(Error::Unavailable)?.address.to_string()
                }),
            )?;
        }
        let client = config
            .mcp
            .server
            .first()
            .map(|server| {
                McpClient::new(
                    server.clone(),
                    Token::new(server.clone(), self.runtime.as_ref().map(|r| r.grant.clone())),
                    Duration::from_secs(config.agent.task_deadline_seconds),
                )
                .map_err(|_| Error::Input)
            })
            .transpose()?;
        let host = Policy::new(
            Mcp {
                client,
                trace: trace.clone(),
            },
            &self.allowed,
            &config.audit.log_dir,
            screen.approvals.clone(),
            screen.presentation.clone(),
        )
        .map_err(|_| Error::Unavailable)?;
        self.host = Some(Local {
            host,
            notes: Notes::new(config.home.as_ref(), trace.clone()),
        });
        self.trace = Some(trace.clone());
        Ok(())
    }

    pub fn grant(&self) -> Option<Arc<bridge_mcp::Grant>> {
        self.runtime.as_ref().map(|runtime| runtime.grant.clone())
    }

    pub async fn catalog(&mut self) -> Result<Catalog, Error> {
        if self.specs.is_none() {
            self.specs = Some(
                self.host
                    .as_ref()
                    .ok_or(Error::Unavailable)?
                    .list_tools()
                    .await
                    .map_err(|_| Error::Unavailable)?,
            );
        }
        // Link can complete through the operator endpoint before the first wake.
        // Discovery occurs once, before thread/start, and never grants authority.
        let mut local = session_specs().to_vec();
        local.extend(self.host.as_ref().ok_or(Error::Unavailable)?.notes.specs());
        local_catalog(
            self.specs.as_ref().ok_or(Error::Unavailable)?.clone(),
            &self.allowed,
            local,
        )
    }

    pub async fn close(&mut self) {
        self.host = None;
        if let Some(runtime) = &mut self.runtime {
            runtime.close().await;
        }
    }
}

/// Append both builtins after discovery. A discovered tool may neither
/// duplicate nor displace either builtin's exact alias.
#[cfg(test)]
pub(super) fn session_catalog(specs: Vec<ToolSpec>, allowed: &[String]) -> Result<Catalog, Error> {
    local_catalog(specs, allowed, session_specs().to_vec())
}

fn local_catalog(specs: Vec<ToolSpec>, allowed: &[String], local: Vec<ToolSpec>) -> Result<Catalog, Error> {
    Catalog::with_local(specs, allowed, local, &NAMES).map_err(|_| Error::Input)
}

pub(super) async fn conversation<L, R, W, P>(
    mut device: (&L, &mut L::Events, <L::Events as echo_link::LinkEvents>::Run),
    io: &mut echo_codex::codex_io::RpcIo<R, W>,
    peer: P,
    options: (super::ConversationOptions<'_>, &mut Tools),
    cancelled: impl std::future::Future<Output = ()>,
    mut observe: impl FnMut(super::SessionNotice) -> Result<(), Error>,
) -> Result<u64, Error>
where
    L: echo_link::EchoLink,
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
    P: echo_media::MediaPeer,
{
    let (options, tools) = options;
    let mut call = crate::bridge_codex::MediaCall::new(io, peer);
    tokio::pin!(cancelled);
    let work = async {
        // Discovery can outlast the chime. Keep its receipt, ordered mute and
        // microphone capture supervised through both discovery and call start.
        let catalog = super::earcon::negotiating(
            device.1,
            super::earcon::capture_during(&mut device.2, async {
                let catalog = tokio::select! {
                    biased;
                    () = cancelled.as_mut() => return Err(Error::Control),
                    result = tokio::time::timeout_at(options.deadline, tools.catalog()) => result.map_err(|_| Error::Deadline)??,
                };
                let trace = tools.trace.as_ref().ok_or(Error::Unavailable)?.clone();
                call.trace_setup(trace.clone())?;
                call.trace_tools(trace, &catalog);
                tokio::select! {
                    biased;
                    () = cancelled.as_mut() => return Err(Error::Control),
                    result = call.start(&options.call, catalog.tools().to_vec(), options.deadline) => result?,
                }
                Ok(catalog)
            }),
            &mut observe,
        )
        .await?;
        observe(super::SessionNotice::Connected)?;
        // One bounded request at a time; the dispatch awaits the run's answer.
        let (requests, receiver) = mpsc::channel(1);
        let (run, active) = tokio::sync::watch::channel(None);
        // Keep the retained scheduler/run future off the Windows main stack.
        Box::pin(super::voice_session::session_conversation(
            device,
            &mut call,
            (
                Session::new(tools.host.as_ref().ok_or(Error::Unavailable)?.clone(), requests, active),
                catalog,
                tools.config.clone(),
                tools.budget.clone(),
            ),
            Some((receiver, run)),
            options.session,
            cancelled.as_mut(),
            &mut observe,
        ))
        .await
    };
    let result = work.await;
    let cleanup = call.close(tokio::time::Instant::now() + Duration::from_secs(2)).await;
    if result == Err(Error::Control) {
        cleanup.and(result)
    } else {
        result.and_then(|completed| cleanup.map(|()| completed))
    }
}
