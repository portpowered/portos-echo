//! One process-owned MCP/policy/ledger lifetime spans every voice call.
use super::tool_policy::Policy;
use crate::{
    Error,
    bridge_codex::ToolBudget,
    bridge_mcp::{self, Token},
    bridge_runtime::bridge_screen::Screen,
    bridge_trace::Trace,
};
use echo_agent_api::{ToolError, ToolHost, ToolResult, ToolSpec};
use echo_config::{AgentConfig, BridgeConfig};
use echo_delegation::codex_dispatch::Catalog;
use echo_mcp_client::McpClient;
use serde_json::Value;
use std::{sync::Arc, time::Duration};

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

pub(super) struct Tools {
    pub host: Option<Policy<Mcp>>,
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
        self.host = Some(host);
        self.trace = Some(trace.clone());
        Ok(())
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
        Catalog::new(self.specs.as_ref().ok_or(Error::Unavailable)?.clone(), &self.allowed).map_err(|_| Error::Input)
    }

    pub async fn close(&mut self) {
        self.host = None;
        if let Some(runtime) = &mut self.runtime {
            runtime.close().await;
        }
    }
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
        let catalog = tokio::select! {
            biased;
            () = cancelled.as_mut() => return Err(Error::Control),
            result = tokio::time::timeout_at(options.deadline, tools.catalog()) => result.map_err(|_| Error::Deadline)??,
        };
        let trace = tools.trace.as_ref().ok_or(Error::Unavailable)?.clone();
        call.trace_setup(trace.clone())?;
        call.trace_tools(trace, &catalog);
        super::earcon::negotiating(
            device.1,
            super::earcon::capture_during(&mut device.2, async {
                tokio::select! {
                    biased;
                    () = cancelled.as_mut() => return Err(Error::Control),
                    result = call.start(&options.call, catalog.tools().to_vec(), options.deadline) => result?,
                }
                Ok(())
            }),
            &mut observe,
        )
        .await?;
        observe(super::SessionNotice::Connected)?;
        // Keep the retained scheduler/run future off the Windows main stack.
        Box::pin(super::tool_conversation(
            device,
            &mut call,
            (
                tools.host.as_ref().ok_or(Error::Unavailable)?.clone(),
                catalog,
                tools.config.clone(),
                tools.budget.clone(),
            ),
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
