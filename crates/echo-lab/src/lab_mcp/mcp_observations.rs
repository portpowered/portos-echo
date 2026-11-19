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
        let result = self.model_inner(delegation, request, false);
        self.invalid |= result.is_err();
        self.valid().and(result)
    }

    /// Observe the pinned app server's dynamic-tool output representation.
    /// # Errors
    /// Unchanged identities must retain their exact content, including errors.
    pub fn codex_model(&mut self, thread: &str, request: &Value) -> Result<(), &'static str> {
        let result = self.model_inner(thread, request, true);
        self.invalid |= result.is_err();
        self.valid().and(result)
    }

    fn model_inner(&mut self, delegation: &str, request: &Value, codex: bool) -> Result<(), &'static str> {
        if delegation.is_empty() {
            return Err(INVALID);
        }
        for item in request["input"].as_array().ok_or(INVALID)? {
            if item["type"] != "function_call_output" {
                continue;
            }
            let id = item["call_id"].as_str().filter(|id| !id.is_empty()).ok_or(INVALID)?;
            let text = item["output"].as_str().ok_or(INVALID)?;
            let failed = codex && matches!(text, "tool execution failed" | "tool authorization required");
            let output: Value = if failed {
                json!(text)
            } else {
                serde_json::from_str(text).map_err(|_| INVALID)?
            };
            let key = (delegation.into(), id.into());
            if let Some(previous) = self.results.get(&key) {
                if previous != &output {
                    return Err(INVALID);
                }
                continue;
            }
            let success = if codex {
                json!([{"text":"m5-linked","type":"text"}])
            } else {
                json!({"content":[{"text":"m5-linked","type":"text"}],"is_error":false})
            };
            if output == success {
                self.counts.tool_call += 1;
                self.reload = match self.reload {
                    Reload::Restarted => Reload::Used,
                    Reload::Reloaded => {
                        self.counts.restart_grant_kept += 1;
                        Reload::Proved
                    }
                    other => other,
                };
            } else if !(failed || !codex && output["is_error"] == true) {
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

/// Device measurement uses completed Port OS receipts, never fake model or
/// resource histories. The trace reader supplies its actual process generation.
pub struct PortosObservations {
    lifecycle: Observations,
    server: String,
    names: BTreeSet<String>,
    generation: u64,
    elapsed: u64,
    catalog: bool,
    attempts: Vec<(String, bool)>,
    identities: BTreeSet<(String, String, String)>,
}

impl PortosObservations {
    /// Record one externally observed invalid_grant after the rotated-token replay.
    /// # Errors
    /// Repeated replay or replay before a complete restart/use is invalid.
    pub fn replay_rejected(&mut self) -> Result<(), &'static str> {
        let counts = self.lifecycle.counts;
        let result = if counts.link == 1
            && counts.forced_refresh == 2
            && counts.tool_call == 1
            && counts.restart_grant_kept == 1
            && counts.replay_rejected == 0
            && self.settled()
        {
            self.lifecycle.counts.replay_rejected = 1;
            Ok(())
        } else {
            Err(INVALID)
        };
        self.finish(result)
    }

    /// # Errors
    /// Requires one configured server and an unambiguous permitted tool set.
    pub fn new(server: &str, names: &[String]) -> Result<Self, &'static str> {
        let unique: BTreeSet<_> = names.iter().cloned().collect();
        if !safe_name(server) || unique.is_empty() || unique.len() != names.len() || !names.iter().all(|n| safe_name(n))
        {
            return Err(INVALID);
        }
        Ok(Self {
            lifecycle: Observations::default(),
            server: server.into(),
            names: unique,
            generation: 0,
            elapsed: 0,
            catalog: false,
            attempts: Vec::new(),
            identities: BTreeSet::new(),
        })
    }

    /// Advance only after the previous bridge has stopped and its trace drained.
    /// # Errors
    /// Skipped/repeated generations or unresolved cancelled calls invalidate proof.
    pub fn restarted(&mut self, generation: u64) -> Result<(), &'static str> {
        let result = if self.generation.checked_add(1) == Some(generation) && self.settled() {
            self.lifecycle.restarted();
            self.generation = generation;
            self.elapsed = 0;
            self.catalog = false;
            self.attempts.clear();
            self.identities.clear();
            Ok(())
        } else {
            Err(INVALID)
        };
        self.finish(result)
    }

    /// Consume each record once, tagged by the reader that owns that process.
    /// # Errors
    /// Stale, duplicate, unmatched or conflicting observations fail permanently.
    pub fn trace(&mut self, generation: u64, value: &Value) -> Result<(), &'static str> {
        let result = self.trace_inner(generation, value);
        self.finish(result)
    }

    fn trace_inner(&mut self, generation: u64, value: &Value) -> Result<(), &'static str> {
        let elapsed = value["elapsed_ms"].as_u64().ok_or(INVALID)?;
        if generation != self.generation || elapsed < self.elapsed {
            return Err(INVALID);
        }
        self.elapsed = elapsed;
        match value["event"].as_str().ok_or(INVALID)? {
            "tools/call" if value["name"].as_str().is_some_and(|name| self.names.contains(name)) => {
                let id = |key| {
                    value[key]
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .map(str::to_owned)
                        .ok_or(INVALID)
                };
                if !self
                    .identities
                    .insert((id("thread_id")?, id("turn_id")?, id("call_id")?))
                {
                    return Err(INVALID);
                }
                self.attempts
                    .push((value["name"].as_str().ok_or(INVALID)?.into(), false));
            }
            "mcp.tools_list.completed" => {
                self.envelope(value, &["server", "outcome", "count"])?;
                if self.catalog || value["outcome"] != "success" || value["count"].as_u64().is_none() {
                    return Err(INVALID);
                }
                self.catalog = true;
                self.lifecycle.counts.tools_list += 1;
            }
            "mcp.tool.completed" => self.completed(value)?,
            _ => self.lifecycle.trace(value)?,
        }
        Ok(())
    }

    fn envelope(&self, value: &Value, fields: &[&str]) -> Result<(), &'static str> {
        let object = value.as_object().ok_or(INVALID)?;
        if value["server"] != self.server
            || !matches!(value["provider_auth_mode"].as_str(), Some("api_key" | "chatgpt"))
            || object.len() != fields.len() + 3
            || !object.keys().all(|key| {
                fields.contains(&key.as_str()) || ["event", "elapsed_ms", "provider_auth_mode"].contains(&key.as_str())
            })
        {
            return Err(INVALID);
        }
        Ok(())
    }

    fn completed(&mut self, value: &Value) -> Result<(), &'static str> {
        self.envelope(value, &["server", "name", "outcome", "attempt_count"])?;
        let ordinal = value["attempt_count"].as_u64().filter(|n| *n > 0).ok_or(INVALID)?;
        let index = usize::try_from(ordinal - 1).map_err(|_| INVALID)?;
        let attempt = self.attempts.get_mut(index).ok_or(INVALID)?;
        if attempt.1 || value["name"] != attempt.0 || !matches!(value["outcome"].as_str(), Some("success" | "error")) {
            return Err(INVALID);
        }
        attempt.1 = true;
        if value["outcome"] == "success" {
            self.lifecycle.counts.tool_call += 1;
            self.lifecycle.reload = match self.lifecycle.reload {
                Reload::Restarted => Reload::Used,
                Reload::Reloaded => {
                    self.lifecycle.counts.restart_grant_kept += 1;
                    Reload::Proved
                }
                other => other,
            };
        }
        Ok(())
    }

    /// Repeated screen deliveries do not count as new transitions.
    pub fn screen(&mut self, state: &UiState) {
        self.lifecycle.screen(state);
    }

    /// Final reconciliation must run after cleanup drains the last trace.
    /// # Errors
    /// Invalid evidence or an attempt without a returned outcome cannot pass.
    pub fn measure(&self) -> Result<LinkCounts, &'static str> {
        self.lifecycle.valid()?;
        if !self.settled() {
            return Err(INVALID);
        }
        Ok(self.lifecycle.counts)
    }

    fn settled(&self) -> bool {
        self.attempts.iter().all(|(_, completed)| *completed)
    }

    fn finish(&mut self, result: Result<(), &'static str>) -> Result<(), &'static str> {
        self.lifecycle.invalid |= result.is_err();
        self.lifecycle.valid().and(result)
    }
}

fn safe_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 256 && name.bytes().all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
}

#[cfg(test)]
#[path = "mcp_observation_tests.rs"]
mod tests;
