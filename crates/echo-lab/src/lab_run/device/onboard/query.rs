//! Only the delegated bridge executes the query; the driver plays nominated speech.
use crate::lab_run::{
    McpAction,
    device::{Configured, runtime::Runtime},
    run_report::Artifacts,
};

const QUERY: &str = "device: inconsistent MCP query observations";

pub(super) struct Query {
    generation: u64,
    offset: usize,
    previous_run: Option<String>,
    successes: u64,
    probe: bool,
}

impl Configured<'_> {
    pub(super) fn mcp_query(
        &mut self,
        action: McpAction,
        index: usize,
        artifacts: &mut Artifacts,
    ) -> Result<(), String> {
        if self.options.wake_clip.is_none() {
            return Err("device: M5 query requires --wake-clip naming only Echo test light".into());
        }
        let query = self.state.begin_mcp_query(action)?;
        self.play(std::path::Path::new(""), index, None, artifacts)?;
        self.state.finish_mcp_query(&query)
    }
}

impl Runtime {
    pub(super) fn begin_mcp_query(&mut self, action: McpAction) -> Result<Query, String> {
        if !matches!(action, McpAction::Call | McpAction::Probe) {
            return Err(QUERY.into());
        }
        if self.pending_mcp.is_some() {
            return Err("device: MCP link already pending".into());
        }
        if self.remote.is_none() || self.muted() || !self.started {
            return Err("device: M5 query requires a running unmuted on-device bridge".into());
        }
        self.observe()?;
        let counts = self.portos.as_ref().ok_or(QUERY)?.measure()?;
        Ok(Query {
            generation: self.generation,
            offset: self.trace.records.len(),
            previous_run: self.run.clone(),
            successes: counts.tool_call,
            probe: action == McpAction::Probe,
        })
    }

    pub(super) fn finish_mcp_query(&mut self, query: &Query) -> Result<(), String> {
        let result = self.check_mcp_query(query);
        self.invalid |= result.is_err();
        result
    }

    fn check_mcp_query(&mut self, query: &Query) -> Result<(), String> {
        self.observe()?;
        let run = self.run.as_deref().ok_or(QUERY)?;
        if self.generation != query.generation || query.previous_run.as_deref() == Some(run) {
            return Err(QUERY.into());
        }
        let records = self.trace.records.get(query.offset..).ok_or(QUERY)?;
        let counts = self.portos.as_ref().ok_or(QUERY)?.measure()?;
        let expected = query.successes.checked_add(u64::from(!query.probe)).ok_or(QUERY)?;
        let attempts: Vec<_> = records
            .iter()
            .filter(|record| record["event"] == "tools/call")
            .collect();
        if counts.tool_call != expected
            || !records
                .iter()
                .any(|record| record["event"] == "bridge.run_completed" && record["run_id"] == run)
            || attempts
                .iter()
                .any(|record| record["name"] != "endpoint_query" || record["run_id"] != run)
            || (!query.probe && attempts.len() != 1)
            || (query.probe && attempts.len() > 1)
        {
            return Err(QUERY.into());
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
