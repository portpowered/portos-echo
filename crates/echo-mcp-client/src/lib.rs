//! Stateless POST-only MCP 2025-03-26 client for the bridge.
//!
//! Construct with a per-server `McpServer`, a `TokenProvider`, and a deadline.
//! Catalog discovery does not grant authority: calls require exact allowlist membership.
//! Explicit repeated calls execute again; the executor owns the durable idempotency ledger.
//! No redirect, proxy inheritance, transport retry, GET stream or session is used.
//! Limits: 16 active operations, 1 MiB per body, 32 catalog pages and 1024 tools.
//! The caller deadline covers initialization, token acquisition and the entire operation.
mod mcp_error;
mod mcp_transport;
mod mcp_wire;

use echo_agent_api::{ToolError, ToolHost, ToolResult, ToolSpec};
use echo_config::McpServer;
pub use mcp_error::McpError;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{OnceCell, Semaphore};

use echo_contracts::secret::Secret;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("token unavailable")]
    Unavailable,
}

pub trait TokenProvider: Send + Sync {
    fn token(&self) -> impl Future<Output = Result<Secret<String>, TokenError>> + Send + '_;

    /// Called before returning an HTTP 401, without retrying the request.
    /// Grant owners can latch rejection before another token acquisition.
    /// Implementations must not block or log credential contents.
    fn unauthorized(&self) {}
}

pub struct McpClient<P> {
    endpoint: reqwest::Url,
    http: reqwest::Client,
    provider: P,
    allowed: HashSet<String>,
    deadline: Duration,
    ids: AtomicU64,
    initialized: OnceCell<()>,
    slots: Semaphore,
}

impl<P: TokenProvider> McpClient<P> {
    /// # Errors
    /// Returns a fixed configuration category for invalid endpoints or zero deadlines.
    pub fn new(server: McpServer, provider: P, deadline: Duration) -> Result<Self, McpError> {
        let endpoint = reqwest::Url::parse(&server.url).map_err(|_| McpError::InvalidConfiguration)?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
            || deadline.is_zero()
        {
            return Err(McpError::InvalidConfiguration);
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .build()
            .map_err(|_| McpError::InvalidConfiguration)?;
        Ok(Self {
            endpoint,
            http,
            provider,
            deadline,
            allowed: server.allow_tools.into_iter().collect(),
            ids: AtomicU64::new(1),
            initialized: OnceCell::new(),
            slots: Semaphore::new(16),
        })
    }

    async fn bounded<T>(&self, future: impl Future<Output = Result<T, McpError>>) -> Result<T, McpError> {
        let _permit = self.slots.try_acquire().map_err(|_| McpError::Capacity)?;
        tokio::time::timeout(self.deadline, future)
            .await
            .map_err(|_| McpError::Timeout)?
    }

    async fn initialize(&self) -> Result<(), McpError> {
        self.initialized
            .get_or_try_init(|| async {
                let result = self
                    .rpc(
                        "initialize",
                        json!({
                            "protocolVersion":"2025-03-26", "capabilities":{},
                            "clientInfo":{"name":"echo-mcp-client","version":"0.1.0"}
                        }),
                    )
                    .await?;
                mcp_wire::initialized(&result)?;
                self.post(json!({"jsonrpc":"2.0","method":"notifications/initialized"}), None)
                    .await?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Discover the complete catalog; discovery never changes the call allowlist.
    /// # Errors
    /// Returns a typed policy, capacity, deadline, HTTP or wire error without remote text.
    pub async fn list(&self) -> Result<Vec<ToolSpec>, McpError> {
        self.bounded(async {
            self.initialize().await?;
            let mut catalog = mcp_wire::Catalog::default();
            let mut params = json!({});
            loop {
                let page = self.rpc("tools/list", params).await?;
                let Some(cursor) = catalog.add(page)? else {
                    return Ok(catalog.finish());
                };
                params = json!({"cursor":cursor});
            }
        })
        .await
    }

    /// Execute exactly one authorized attempt. Cancellation does not undo remote effects.
    /// # Errors
    /// Refusal occurs before token acquisition, initialization or any network request.
    pub async fn call(&self, name: &str, args: Value) -> Result<ToolResult, McpError> {
        if !self.allowed.contains(name) {
            return Err(McpError::Refused);
        }
        if !args.is_object() {
            return Err(McpError::Protocol);
        }
        self.bounded(async {
            self.initialize().await?;
            mcp_wire::result(&self.rpc("tools/call", json!({"name":name,"arguments":args})).await?)
        })
        .await
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self
            .ids
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| McpError::Capacity)?;
        self.post(
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            Some(id),
        )
        .await
    }
}

impl<P: TokenProvider> ToolHost for McpClient<P> {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>, ToolError> {
        self.list().await.map_err(Into::into)
    }
    async fn call_tool(&self, name: &str, args: Value, _idempotency_key: &str) -> Result<ToolResult, ToolError> {
        self.call(name, args).await.map_err(Into::into)
    }
}

#[cfg(test)]
mod mcp_tests;
