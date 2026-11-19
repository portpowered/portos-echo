//! One process-owned MCP/policy/ledger lifetime spans every voice call.
use super::session_tools::{END, EXTEND};
use super::tool_policy::Policy;
use crate::{
    Error,
    bridge_codex::{SessionCommand, SessionRequest, ToolBudget},
    bridge_mcp::{self, Token},
    bridge_runtime::bridge_screen::Screen,
    bridge_trace::{PortosReceipts, Trace},
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
    receipts: Option<PortosReceipts>,
}

impl Mcp {
    fn new(client: Option<McpClient<Token>>, trace: Trace, server: Option<&echo_config::McpServer>) -> Self {
        let receipts = server
            .filter(|server| server.auth == echo_config::McpAuth::PortosDeviceCode)
            .map(|server| PortosReceipts::new(trace.clone(), server.name.clone()));
        Self {
            client,
            trace,
            receipts,
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
        let result = client.call_tool(name, args, key).await;
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
                let ca_pem = if server.auth == echo_config::McpAuth::PortosDeviceCode {
                    bridge_mcp::Settings::ca_pem(config)?
                } else {
                    None
                };
                McpClient::with_ca(
                    server.clone(),
                    Token::new(server.clone(), self.runtime.as_ref().map(|r| r.grant.clone())),
                    Duration::from_secs(config.agent.task_deadline_seconds),
                    ca_pem.as_deref(),
                )
                .map_err(|_| Error::Input)
            })
            .transpose()?;
        let host = Policy::new(
            Mcp::new(client, trace.clone(), config.mcp.server.first()),
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
        session_catalog(self.specs.as_ref().ok_or(Error::Unavailable)?.clone(), &self.allowed)
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
pub(super) fn session_catalog(mut specs: Vec<ToolSpec>, allowed: &[String]) -> Result<Catalog, Error> {
    specs.extend(session_specs());
    let mut allowed = allowed.to_vec();
    allowed.extend([EXTEND.to_owned(), END.to_owned()]);
    let catalog = Catalog::new(specs, &allowed).map_err(|_| Error::Input)?;
    if [EXTEND, END].iter().any(|name| catalog.resolve(name) != Some(*name)) {
        return Err(Error::Input);
    }
    Ok(catalog)
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

#[cfg(test)]
mod mcp_trust_tests {
    use super::*;
    use crate::bridge_mcp::bridge_component::{
        TlsPeer, approve, assert_mcp_trust_refused, assert_private, assert_tool_receipts, drive,
    };

    #[tokio::test(start_paused = true)]
    async fn t3_mcp_configured_https_link_catalog_query_and_reload() {
        drive(async {
            let peer = TlsPeer::start(false).await;
            let config = &peer.config;
            let screen = Screen::bind(None).await.unwrap();
            let mut trace = Trace::open(&peer.directory.path().join("trace"), "api_key").unwrap();
            let mut tools = Tools::new(config, Arc::new(|| 0));
            tools.open(config, &screen, &mut trace, None).await.unwrap();
            approve(tools.runtime.as_ref().unwrap()).await;
            let path = config.mcp.state_dir.as_ref().unwrap().join("grant.json");
            let stored = std::fs::read(&path).unwrap();
            assert_private(&path);
            let host = tools.host.as_ref().unwrap();
            let specs = host.list_tools().await.unwrap();
            assert_eq!(specs.len(), 1);
            assert_eq!(specs[0].name, "endpoint_query");
            assert_eq!(specs[0].input_schema, json!({"type":"object","properties":{}}));
            let result = host.call_tool("endpoint_query", json!({}), "1:t1:u1:c").await.unwrap();
            assert!(!result.is_error);
            assert_eq!(result.content, json!([{"type":"text","text":"synthetic-query-result"}]));
            assert_mcp_trust_refused(tools.runtime.as_ref().unwrap(), config).await;
            tools.close().await;
            tools.open(config, &screen, &mut trace, None).await.unwrap();
            assert_eq!(
                tools.host.as_ref().unwrap().list_tools().await.unwrap()[0].name,
                "endpoint_query"
            );
            assert_eq!(std::fs::read(path).unwrap(), stored);
            tools.close().await;
            assert_eq!(peer.finish().await, json!({"link":1,"token":1,"catalog":2,"query":1}));
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn t3_mcp_receipts_count_returns_exclude_payloads_and_never_replay() {
        drive(async {
            let mut peer = TlsPeer::start(false).await;
            let screen = Screen::bind(None).await.unwrap();
            let path = peer.directory.path().join("trace");
            let mut trace = Trace::open(&path, "api_key").unwrap();
            let mut tools = Tools::new(&peer.config, Arc::new(|| 0));
            tools.open(&peer.config, &screen, &mut trace, None).await.unwrap();
            approve(tools.runtime.as_ref().unwrap()).await;
            tools.catalog().await.unwrap();
            tools.catalog().await.unwrap(); // The process catalog is cached.
            let host = tools.host.as_ref().unwrap();
            let key = "1:t1:u1:c";
            let result = host.call_tool("endpoint_query", json!({}), key).await.unwrap();
            assert_eq!(result.content, json!([{"type":"text","text":"synthetic-query-result"}]));
            assert!(!result.is_error);
            assert_eq!(
                host.call_tool("endpoint_query", json!({}), key).await,
                Err(ToolError::Denied)
            );
            assert!(
                host.call_tool("endpoint_query", json!({"mode":"error"}), "1:t1:u1:e")
                    .await
                    .unwrap()
                    .is_error
            );
            for (mode, key, error) in [
                ("http", "1:t1:u1:h", ToolError::Server("http_status:503".into())), // lint-rules: allow-literal typed HTTP status assertion, never a network address
                ("malformed", "1:t1:u1:m", ToolError::Transport),
            ] {
                assert_eq!(
                    host.call_tool("endpoint_query", json!({"mode":mode}), key).await,
                    Err(error)
                );
            }
            {
                let pending = host.call_tool("endpoint_query", json!({"mode":"pending"}), "1:t1:u1:p");
                tokio::pin!(pending);
                tokio::select! {
                    () = peer.pending() => (),
                    result = &mut pending => panic!("pending tool returned: {result:?}"),
                }
            } // Cancellation drops the actual in-flight HTTP future.
            tools.close().await;
            assert_tool_receipts(
                &path,
                &["c", "e", "h", "m", "p"],
                &[(1, "success"), (2, "error"), (3, "error"), (4, "error")],
            );
            assert_eq!(peer.finish().await, json!({"link":1,"token":1,"catalog":1,"query":5}));
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn t3_mcp_receipts_correlate_overlapping_calls_completed_out_of_order() {
        drive(async {
            let mut peer = TlsPeer::start(false).await;
            let screen = Screen::bind(None).await.unwrap();
            let path = peer.directory.path().join("trace");
            let mut trace = Trace::open(&path, "api_key").unwrap();
            let mut tools = Tools::new(&peer.config, Arc::new(|| 0));
            tools.open(&peer.config, &screen, &mut trace, None).await.unwrap();
            approve(tools.runtime.as_ref().unwrap()).await;
            tools.catalog().await.unwrap();
            let host = tools.host.as_ref().unwrap();
            {
                let first = host.call_tool("endpoint_query", json!({"mode":"pending"}), "1:t1:u1:a");
                tokio::pin!(first);
                tokio::select! {
                    () = peer.pending() => (),
                    result = &mut first => panic!("pending tool returned: {result:?}"),
                }
                assert!(
                    !host
                        .call_tool("endpoint_query", json!({}), "1:t1:u1:b")
                        .await
                        .unwrap()
                        .is_error
                );
                peer.release().await;
                assert!(!first.await.unwrap().is_error);
            }
            tools.close().await;
            assert_tool_receipts(&path, &["a", "b"], &[(2, "success"), (1, "success")]);
            assert_eq!(peer.finish().await, json!({"link":1,"token":1,"catalog":1,"query":2}));
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn t3_mcp_configured_oauth_refuses_missing_invalid_untrusted_and_wrong_hostname() {
        drive(async {
            let mut peer = TlsPeer::start(false).await;
            let unrelated = TlsPeer::start(false).await;
            let wrong_host = TlsPeer::start(true).await;
            let trusted = peer.config.codex.ca_file.clone();
            let invalid = peer.directory.path().join("invalid.pem");
            std::fs::write(&invalid, b"invalid PEM").unwrap();
            let screen = Screen::bind(None).await.unwrap();
            let mut trace = Trace::open(&peer.directory.path().join("trace"), "api_key").unwrap();
            for ca in [
                None,
                Some(invalid),
                unrelated.config.codex.ca_file.clone(),
                Some(peer.directory.path().join("missing.pem")),
            ] {
                peer.config.codex.ca_file = ca;
                let mut tools = Tools::new(&peer.config, Arc::new(|| 0));
                assert!(matches!(
                    tools.open(&peer.config, &screen, &mut trace, None).await,
                    Err(Error::Input | Error::Mcp(echo_mcp_auth::AuthError::Transport))
                ));
                assert!(!peer.config.mcp.state_dir.as_ref().unwrap().exists());
                tools.close().await;
            }
            peer.config.codex.ca_file = trusted;
            let mut tools = Tools::new(&wrong_host.config, Arc::new(|| 0));
            assert!(matches!(
                tools.open(&wrong_host.config, &screen, &mut trace, None).await,
                Err(Error::Mcp(echo_mcp_auth::AuthError::Transport))
            ));
            assert!(!wrong_host.config.mcp.state_dir.as_ref().unwrap().exists());
            tools.close().await;
            let zero = json!({"link":0,"token":0,"catalog":0,"query":0});
            assert_eq!(peer.finish().await, zero);
            assert_eq!(unrelated.finish().await, zero);
            assert_eq!(wrong_host.finish().await, zero);
        })
        .await;
    }
}
