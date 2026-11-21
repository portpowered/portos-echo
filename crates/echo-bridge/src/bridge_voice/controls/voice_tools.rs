//! One process-owned MCP/policy/ledger lifetime spans every voice call.
use super::session_tools::{END, EXTEND};
use super::tool_policy::Policy;
use crate::{
    Error,
    bridge_codex::{SessionCommand, SessionRequest, ToolBudget},
    bridge_mcp::{self, Token},
    bridge_notes::{Local, NAMES, Notes},
    bridge_runtime::bridge_screen::Screen,
    bridge_settings::portos_permissions::PortosPermissions,
    bridge_trace::{PortosReceipts, Trace},
};
use echo_agent_api::{ToolError, ToolHost, ToolResult, ToolSpec};
use echo_config::BridgeConfig;
use echo_delegation::codex_dispatch::Catalog;
use echo_mcp_client::{McpClient, McpError};
use echo_screen_contract::settings_v1::LinkStatusV1;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot};
#[path = "voice_tools/runner_skills.rs"]
mod runner_skills;
use runner_skills::READ_SKILL;
pub use runner_skills::RunnerSkills;
pub(crate) use runner_skills::normalize_skill_arguments;

/// Bounds Port OS catalog discovery well inside the call's connect deadline.
const DISCOVERY: Duration = Duration::from_secs(5);
const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) struct Mcp {
    client: Option<McpClient<Token>>,
    trace: Trace,
    receipts: Option<PortosReceipts>,
    grant: Option<Arc<bridge_mcp::Grant>>,
    portos_permissions: Option<PortosPermissions>,
}

impl Mcp {
    fn new(
        client: Option<McpClient<Token>>,
        trace: Trace,
        server: Option<&echo_config::McpServer>,
        grant: Option<Arc<bridge_mcp::Grant>>,
        portos_permissions: Option<PortosPermissions>,
    ) -> Self {
        let receipts = server
            .filter(|server| server.auth == echo_config::McpAuth::PortosDeviceCode)
            .map(|server| PortosReceipts::new(trace.clone(), server.name.clone()));
        Self {
            client,
            trace,
            receipts,
            grant,
            portos_permissions,
        }
    }

    async fn call(&self, client: &McpClient<Token>, name: &str, args: Value) -> Result<ToolResult, ToolError> {
        let Some(grant) = &self.grant else {
            return client.call(name, args).await.map_err(Into::into);
        };
        if let Some(permissions) = &self.portos_permissions
            && permissions.managed()
        {
            let scopes = grant.granted_scopes().await;
            if !permissions.permits(name, scopes.as_deref()) {
                return Err(ToolError::Denied);
            }
        }
        let generation = grant.scope_generation().map_err(|_| ToolError::Transport)?;
        match client.call(name, args).await {
            Err(McpError::InsufficientScope { scope }) => {
                grant
                    .scope_denied(generation, name, scope)
                    .map_err(|_| ToolError::Transport)?;
                Ok(ToolResult {
                    content: json!([{"type":"text","text": format!(
                        "mcp.scope_insufficient: {name} requires {scope}. Link Echo to Port OS again to control devices."
                    )}]),
                    is_error: true,
                })
            }
            result => result.map_err(Into::into),
        }
    }
}

impl ToolHost for Mcp {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>, ToolError> {
        match &self.client {
            Some(host) => {
                let tools = host.list_tools().await?;
                if let Some(receipts) = &self.receipts {
                    receipts.catalog(tools.len()).map_err(|_| ToolError::Transport)?;
                }
                Ok(tools)
            }
            None => Ok(vec![]),
        }
    }
    async fn call_tool(&self, name: &str, args: Value, key: &str) -> Result<ToolResult, ToolError> {
        let client = self.client.as_ref().ok_or(ToolError::Denied)?;
        let ordinal = if let Some(receipts) = &self.receipts {
            receipts.attempt(name, key).map_err(|_| ToolError::Transport)?
        } else {
            self.trace
                .clone()
                .tool_attempt(name, key)
                .map_err(|_| ToolError::Transport)?;
            0
        };
        let result = self.call(client, name, args).await;
        if let Some(receipts) = &self.receipts {
            receipts
                .complete(name, ordinal, result.as_ref().is_ok_and(|result| !result.is_error))
                .map_err(|_| ToolError::Transport)?;
        }
        result
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
    skills: RunnerSkills,
    requests: mpsc::Sender<SessionRequest>,
    run: tokio::sync::watch::Receiver<Option<Arc<()>>>,
}

impl<H> Session<H> {
    pub(super) fn new(
        host: H,
        skills: RunnerSkills,
        requests: mpsc::Sender<SessionRequest>,
        run: tokio::sync::watch::Receiver<Option<Arc<()>>>,
    ) -> Self {
        Self {
            host,
            skills,
            requests,
            run,
        }
    }
}

impl<H: ToolHost + Sync> ToolHost for Session<H> {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>, ToolError> {
        self.host.list_tools().await
    }
    async fn call_tool(&self, name: &str, args: Value, key: &str) -> Result<ToolResult, ToolError> {
        // Raw arguments never leave this host; only the parsed command does.
        let command = match name {
            READ_SKILL if self.skills.is_enabled() => return Ok(self.skills.read(&args)),
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
    pub budget: ToolBudget,
    pub(super) skills: RunnerSkills,
    muse: Option<Arc<crate::bridge_muse::Backend>>,
    allowed: Vec<String>,
    server: Option<String>,
    specs: Option<Vec<ToolSpec>>,
    runtime: Option<bridge_mcp::Runtime>,
    shared_grant: Option<Arc<bridge_mcp::Grant>>,
    portos_permissions: Option<PortosPermissions>,
    trace: Option<Trace>,
}

impl Tools {
    pub fn new(config: &BridgeConfig, utc_day: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        Self {
            host: None,
            budget: ToolBudget::new(config.agent.daily_request_cap, move || Ok(utc_day())),
            skills: RunnerSkills::default(),
            muse: None,
            allowed: config
                .mcp
                .server
                .first()
                .map_or_else(Vec::new, |server| server.allow_tools.clone()),
            specs: None,
            server: config.mcp.server.first().map(|server| server.name.clone()),
            runtime: None,
            shared_grant: None,
            portos_permissions: None,
            trace: None,
        }
    }

    pub(super) fn with_muse(mut self, config: &BridgeConfig, local: bool) -> Self {
        self.muse = config
            .muse
            .as_ref()
            .and_then(|muse| crate::bridge_muse::Backend::open(muse, local).ok())
            .map(Arc::new);
        self
    }

    pub(super) fn with_portos_permissions(mut self, permissions: Option<PortosPermissions>) -> Self {
        self.portos_permissions = permissions;
        self
    }

    pub(super) fn with_runner_skills(mut self, skills: Option<&RunnerSkills>) -> Self {
        self.skills = skills.cloned().unwrap_or_default();
        self
    }

    pub async fn open(
        &mut self,
        config: &BridgeConfig,
        screen: &Screen,
        trace: &mut Trace,
        epoch: Option<u64>,
    ) -> Result<(), Error> {
        if self.shared_grant.is_none() {
            self.runtime = open_portos(config, screen, trace, epoch).await?;
        }
        let client = config
            .mcp
            .server
            .first()
            .map(|server| {
                let ca_pem = if server.auth == echo_config::McpAuth::PortosDeviceCode {
                    bridge_mcp::Settings::ca_pem(config)?
                } else {
                    None
                };
                McpClient::with_ca(
                    server.clone(),
                    Token::new(server.clone(), self.grant()),
                    MCP_REQUEST_TIMEOUT,
                    ca_pem.as_deref(),
                )
                .map_err(|_| Error::Input)
            })
            .transpose()?;
        let host = Policy::new(
            Mcp::new(
                client,
                trace.clone(),
                config.mcp.server.first(),
                self.grant(),
                self.portos_permissions.clone(),
            ),
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
        self.shared_grant
            .clone()
            .or_else(|| self.runtime.as_ref().map(|runtime| runtime.grant.clone()))
    }

    pub(super) fn share_grant(&mut self, grant: Arc<bridge_mcp::Grant>) {
        self.shared_grant = Some(grant);
    }

    pub async fn catalog(&mut self) -> Result<Catalog, Error> {
        if self.specs.is_none() {
            let host = self.host.as_ref().ok_or(Error::Unavailable)?;
            self.specs = match self.grant() {
                Some(grant) => self.discover_portos(host, &grant).await?,
                None => Some(host.list_tools().await.map_err(|_| Error::Unavailable)?),
            };
        }
        // Link can complete through the operator endpoint before the first wake.
        // Discovery occurs once, before thread/start, and never grants authority.
        let mut local = session_specs().to_vec();
        local.extend(self.host.as_ref().ok_or(Error::Unavailable)?.notes.specs());
        local.extend(self.skills.spec());
        local.extend(self.muse.as_ref().map(|_| crate::bridge_muse::tool_spec()));
        local_catalog(self.specs.clone().unwrap_or_default(), &self.allowed, local)
    }

    /// An unlinked, revoked or unreachable Port OS never stops voice: this call
    /// advertises no server tools and the next call discovers again. No token
    /// is requested without a usable grant, so nothing is rejected on screen.
    /// Only the safe category is traced; no remote text or token.
    async fn discover_portos(
        &self,
        host: &Local<Policy<Mcp>>,
        grant: &bridge_mcp::Grant,
    ) -> Result<Option<Vec<ToolSpec>>, Error> {
        let reason = if grant.link_status() == LinkStatusV1::Linked {
            match tokio::time::timeout(DISCOVERY, host.list_tools()).await {
                Ok(Ok(specs)) => return Ok(Some(specs)),
                Ok(Err(ToolError::Unauthorized)) => "unauthorized",
                Ok(Err(ToolError::Timeout)) | Err(_) => "timeout",
                Ok(Err(_)) => "unavailable",
            }
        } else {
            "unlinked"
        };
        self.trace.clone().ok_or(Error::Unavailable)?.record(
            "mcp.tools_list.unavailable",
            None,
            json!({"server":self.server,"reason":reason}),
        )?;
        Ok(None)
    }

    pub async fn close(&mut self) {
        self.host = None;
        if let Some(runtime) = &mut self.runtime {
            runtime.close().await;
        }
    }
}

pub(super) async fn open_portos(
    config: &BridgeConfig,
    screen: &Screen,
    trace: &mut Trace,
    epoch: Option<u64>,
) -> Result<Option<bridge_mcp::Runtime>, Error> {
    let Some(settings) = bridge_mcp::Settings::configured(config)? else {
        return Ok(None);
    };
    let mut runtime = bridge_mcp::Runtime::open(settings, screen.presentation.clone(), trace.clone(), epoch).await?;
    if let Err(error) = trace.record(
        "mcp.operator.listening",
        None,
        json!({"address":runtime.address.to_string()}),
    ) {
        runtime.close().await;
        return Err(error);
    }
    Ok(Some(runtime))
}

/// Append both builtins after discovery. A discovered tool may neither
/// duplicate nor displace either builtin's exact alias.
#[cfg(test)]
pub(super) fn session_catalog(specs: Vec<ToolSpec>, allowed: &[String]) -> Result<Catalog, Error> {
    local_catalog(specs, allowed, session_specs().to_vec())
}

fn local_catalog(specs: Vec<ToolSpec>, allowed: &[String], local: Vec<ToolSpec>) -> Result<Catalog, Error> {
    let mut reserved = NAMES.to_vec();
    reserved.push(crate::bridge_muse::TOOL);
    if local.iter().any(|spec| spec.name == READ_SKILL) {
        reserved.push(READ_SKILL);
    }
    Catalog::with_local(specs, allowed, local, &reserved).map_err(|_| Error::Input)
}

pub(super) async fn conversation<L, R, W, P>(
    mut device: (&L, &mut L::Events, <L::Events as echo_link::LinkEvents>::Run),
    io: &mut echo_codex::codex_io::RpcIo<R, W>,
    peer: P,
    options: (super::ConversationOptions<'_>, &mut Tools, tokio::time::Instant),
    cancelled: impl std::future::Future<Output = ()>,
    mut observe: impl FnMut(super::SessionNotice) -> Result<(), Error>,
) -> Result<u64, Error>
where
    L: echo_link::EchoLink,
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
    P: echo_media::MediaPeer,
{
    let (options, tools, opened) = options;
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
                call.trace_tools(trace, &catalog, tools.server.as_deref());
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
                Session::new(
                    tools.host.as_ref().ok_or(Error::Unavailable)?.clone(),
                    tools.skills.clone(),
                    requests,
                    active,
                ),
                catalog,
                tools.budget.clone(),
            ),
            Some((
                receiver,
                run,
                tools.skills.is_enabled(),
                tools
                    .muse
                    .clone()
                    .zip(tools.trace.clone())
                    .map(|(backend, trace)| crate::bridge_codex::MuseTool::new(backend, trace)),
            )),
            (options.session, opened),
            cancelled.as_mut(),
            &mut observe,
        ))
        .await
    };
    let result = work.await;
    let cleanup = call.close(tokio::time::Instant::now() + Duration::from_secs(2)).await;
    if matches!(result, Err(Error::Control | Error::SessionCeiling)) {
        cleanup.and(result)
    } else {
        result.and_then(|completed| cleanup.map(|()| completed))
    }
}

#[cfg(test)]
#[path = "voice_tools/mcp_trust_tests.rs"]
mod mcp_trust_tests;

#[cfg(test)]
#[path = "voice_tools/muse_tests.rs"]
mod muse_tests;
