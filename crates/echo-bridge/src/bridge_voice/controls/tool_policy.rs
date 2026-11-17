//! The sole canonical MCP effect boundary, independent of model intent.
use super::tool_ledger::Ledger;
use crate::{bridge_mcp::bridge_presentation::Presentation, bridge_runtime::bridge_screen::approval::Approvals};
use echo_agent_api::{ToolError, ToolHost, ToolResult, ToolSpec};
use serde_json::Value;
use std::{
    collections::HashSet,
    path::Path,
    sync::{Arc, Mutex},
};

pub(crate) struct Policy<H>(Arc<State<H>>);

struct State<H> {
    host: H,
    allowed: HashSet<String>,
    ledger: Mutex<Ledger>,
    approvals: Approvals,
    presentation: Presentation,
}

impl<H> Policy<H> {
    pub fn new(
        host: H,
        allowed: &[String],
        directory: &Path,
        approvals: Approvals,
        presentation: Presentation,
    ) -> Result<Self, ToolError> {
        Ok(Self(Arc::new(State {
            host,
            allowed: allowed.iter().cloned().collect(),
            ledger: Mutex::new(Ledger::open(directory)?),
            approvals,
            presentation,
        })))
    }
}

impl<H> Clone for Policy<H> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<H: ToolHost + Send + Sync> ToolHost for Policy<H> {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>, ToolError> {
        self.0.host.list_tools().await
    }

    async fn call_tool(&self, name: &str, args: Value, key: &str) -> Result<ToolResult, ToolError> {
        let this = &self.0;
        if !this.allowed.contains(name) || !args.is_object() {
            return Err(ToolError::Denied);
        }
        this.ledger.lock().map_err(|_| ToolError::Transport)?.reserve(key)?;
        if name == "test.requires_approval"
            && !this
                .approvals
                .request(&this.presentation)
                .await
                .map_err(|_| ToolError::Denied)?
        {
            return Err(ToolError::Denied);
        }
        let result = this.host.call_tool(name, args, key).await?;
        if let Some((state, deadline)) =
            crate::bridge_runtime::bridge_screen::emotion::decode(name, &result.content, result.is_error)
        {
            this.presentation
                .emotion(state, deadline)
                .map_err(|_| ToolError::Transport)?;
        }
        Ok(result)
    }
}

#[cfg(test)]
#[path = "tool_policy_tests.rs"]
mod tests;
