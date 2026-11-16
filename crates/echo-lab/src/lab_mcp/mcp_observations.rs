//! M5 measurements consume independent runtime observations, never expectations.
use std::collections::{BTreeMap, BTreeSet};

use echo_screen_contract::UiState;
use echo_spoof_mcp::Snapshot;
use serde_json::{Value, json};

use super::Counts;

/// Successful observed lifecycle outcomes, separate from attempted requests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkCounts {
    pub link: u64,
    pub tools_list: u64,
    pub tool_call: u64,
    pub forced_refresh: u64,
    pub replay_rejected: u64,
    pub relink_error: u64,
    pub restart_grant_kept: u64,
    pub unlink: u64,
}

/// One observer per scenario. Feed each trace record once, resetting process
/// correlation only after an actual bridge restart. Model histories may repeat.
#[derive(Default)]
pub struct Observations {
    counts: LinkCounts,
    operations: BTreeSet<u64>,
    pending_links: BTreeSet<u64>,
    results: BTreeMap<(String, String), Value>,
    last_screen: Option<UiState>,
    reload: Reload,
    invalid: bool,
}

#[derive(Clone, Copy, Default)]
enum Reload {
    #[default]
    Initial,
    Restarted,
    Reloaded,
    Used,
    Proved,
}

const INVALID: &str = "M5 observations: inconsistent runtime evidence";
const RELINK: &str = "Run echo-bridge link with your config to relink Port OS MCP.";

impl Observations {
    /// Call after collecting the previous process and before consuming the new
    /// process's trace. Operation IDs and elapsed times are process scoped.
    pub fn restarted(&mut self) {
        self.operations.clear();
        self.pending_links.clear();
        self.results.clear();
        self.last_screen = None;
        self.reload = Reload::Restarted;
    }

    /// Observe a complete bridge trace record. Unknown events do not count.
    /// # Errors
    /// Malformed, duplicate or uncorrelated lifecycle evidence stays invalid.
    pub fn trace(&mut self, value: &Value) -> Result<(), &'static str> {
        let result = self.trace_inner(value);
        self.invalid |= result.is_err();
        self.valid().and(result)
    }

    fn trace_inner(&mut self, value: &Value) -> Result<(), &'static str> {
        let event = value["event"].as_str().ok_or(INVALID)?;
        if event == "mcp.grant.reloaded" {
            self.reload = match self.reload {
                Reload::Restarted => Reload::Reloaded,
                Reload::Used => {
                    self.counts.restart_grant_kept += 1;
                    Reload::Proved
                }
                _ => return Err(INVALID),
            };
        }
        let counter = match event {
            "mcp.link.started" => {
                let id = operation(value)?;
                if self.operations.contains(&id) || !self.pending_links.insert(id) {
                    return Err(INVALID);
                }
                return Ok(());
            }
            "mcp.link.approved" => {
                if !self.pending_links.remove(&operation(value)?) {
                    return Err(INVALID);
                }
                &mut self.counts.link
            }
            "mcp.refresh.committed" => &mut self.counts.forced_refresh,
            "mcp.unlink.completed" => &mut self.counts.unlink,
            _ => return Ok(()),
        };
        let id = operation(value)?;
        if !self.operations.insert(id) || self.pending_links.contains(&id) {
            return Err(INVALID);
        }
        *counter += 1;
        Ok(())
    }

    /// Observe decoded screen states, counting transitions to the exact MCP
    /// relink instruction. Repeated SSE deliveries do not create new outcomes.
    pub fn screen(&mut self, state: &UiState) {
        if self.last_screen.as_ref() != Some(state)
            && matches!(state, UiState::Error { message, relink: true } if message == RELINK)
        {
            self.counts.relink_error += 1;
        }
        self.last_screen = Some(state.clone());
    }

    /// Consume the real delegated model request's function outputs. A spoof
    /// arrival alone cannot prove success: its fault modes record calls too.
    /// # Errors
    /// Malformed or changed results invalidate measurement without payload logs.
    pub fn model(&mut self, delegation: &str, request: &Value) -> Result<(), &'static str> {
        let result = self.model_inner(delegation, request);
        self.invalid |= result.is_err();
        self.valid().and(result)
    }

    fn model_inner(&mut self, delegation: &str, request: &Value) -> Result<(), &'static str> {
        if delegation.is_empty() {
            return Err(INVALID);
        }
        for item in request["input"].as_array().ok_or(INVALID)? {
            if item["type"] != "function_call_output" {
                continue;
            }
            let id = item["call_id"].as_str().filter(|id| !id.is_empty()).ok_or(INVALID)?;
            let output: Value = serde_json::from_str(item["output"].as_str().ok_or(INVALID)?).map_err(|_| INVALID)?;
            let key = (delegation.into(), id.into());
            if let Some(previous) = self.results.get(&key) {
                if previous != &output {
                    return Err(INVALID);
                }
                continue;
            }
            if output == json!({"content":[{"text":"m5-linked","type":"text"}],"is_error":false}) {
                self.counts.tool_call += 1;
                self.reload = match self.reload {
                    Reload::Restarted => Reload::Used,
                    Reload::Reloaded => {
                        self.counts.restart_grant_kept += 1;
                        Reload::Proved
                    }
                    other => other,
                };
            } else if output["is_error"] != true {
                return Err(INVALID);
            }
            self.results.insert(key, output);
        }
        Ok(())
    }

    /// Reconcile independent fixture and resource observations. Call again after
    /// cleanup so late events cannot escape the scenario's exact-count oracle.
    /// # Errors
    /// Missing signed admission or contradictory successes never yield counts.
    pub fn measure(&self, auth: Counts, resource: &Snapshot) -> Result<LinkCounts, &'static str> {
        self.valid()?;
        if auth.links < self.counts.link
            || auth.refreshes < self.counts.forced_refresh
            || resource
                .requests
                .iter()
                .any(|r| r.dpop_header || r.authorization_matches.is_none())
        {
            return Err(INVALID);
        }
        let accepted = |method: &str| {
            resource
                .requests
                .iter()
                .filter(|r| r.method == method && r.authorization_matches == Some(true))
                .count() as u64
        };
        let dispatched = resource
            .calls
            .iter()
            .filter(|r| r.name == "test.echo" && r.args == json!({"text":"m5-linked"}))
            .count() as u64;
        if self.counts.tool_call > dispatched || self.counts.tool_call > accepted("tools/call") {
            return Err(INVALID);
        }
        Ok(LinkCounts {
            tools_list: accepted("tools/list"),
            replay_rejected: auth.replay_rejected,
            ..self.counts
        })
    }

    fn valid(&self) -> Result<(), &'static str> {
        if self.invalid { Err(INVALID) } else { Ok(()) }
    }
}

fn operation(value: &Value) -> Result<u64, &'static str> {
    value["operation_id"].as_u64().filter(|id| *id > 0).ok_or(INVALID)
}

#[cfg(test)]
#[path = "mcp_observation_tests.rs"]
mod tests;
