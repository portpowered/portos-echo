//! Settings for an isolated, client-tool-only pinned Codex app-server thread.
//! Wire adaptation, bounded request bookkeeping and asynchronous stream IO.
//! Explicit process generations with private state and bounded restart.
pub mod codex_codec;
pub mod codex_io;
pub mod codex_protocol;
pub mod codex_rpc;
pub mod codex_supervisor;
use serde::Serialize;
use serde_json::{Value, json};

/// A bridge-owned tool. Its arguments and result remain owned by its caller.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DynamicTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// The minimal thread/start surface, adapted from the pinned generated schema.
// OpenAI Codex, Copyright 2025 OpenAI; Apache-2.0 (see ../NOTICE).
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSettings {
    ephemeral: bool,
    approval_policy: &'static str,
    sandbox: &'static str,
    environments: Vec<Value>,
    dynamic_tools: Vec<DynamicTool>,
    config: Value,
}

impl ThreadSettings {
    /// Disable built-in execution tools and preserve exactly the caller's tools.
    #[must_use]
    pub fn new(dynamic_tools: Vec<DynamicTool>) -> Self {
        Self {
            ephemeral: true,
            approval_policy: "never",
            sandbox: "read-only",
            environments: Vec::new(),
            dynamic_tools,
            config: json!({
                "features.view_image": false,
                "features.shell_tool": false,
                "features.unified_exec": false,
                "features.multi_agent": false,
                "features.multi_agent_v2": false,
                "agents.enabled": false,
                "features.goals": false,
                "tools.experimental_request_user_input.enabled": false,
                "tools.update_plan.enabled": false,
                // Every catalog model is code-mode-only. `functions` is the pin's
                // namespace for the caller's tools: declared directly they need no
                // code-mode host, which the Echo's install does not have.
                "features.code_mode.direct_only_tool_namespaces": ["web", "functions"],
                "web_search": "live",
                "mcp_servers": {},
                "skills.include_instructions": false,
            }),
        }
    }
    /// Adapt the caller's validated bridge mode without a config-crate dependency.
    #[must_use]
    pub fn with_web_search(mut self, mode: &str) -> Self {
        self.config["web_search"] = mode.into();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_search_changes_only_the_selected_config_field() {
        for mode in ["live", "cached", "disabled"] {
            let tool = DynamicTool {
                name: "test_echo".into(),
                description: "Echo".into(),
                input_schema: json!({"type":"object"}),
            };
            for tools in [vec![], vec![tool]] {
                let mut expected = serde_json::to_value(ThreadSettings::new(tools.clone())).unwrap();
                expected["config"]["web_search"] = mode.into();
                assert_eq!(
                    serde_json::to_value(ThreadSettings::new(tools).with_web_search(mode)).unwrap(),
                    expected
                );
            }
        }
    }

    #[test]
    fn thread_settings_preserve_empty_and_nonempty_caller_tools() -> Result<(), serde_json::Error> {
        let empty = serde_json::to_value(ThreadSettings::new(vec![]))?;
        assert_eq!(empty["dynamicTools"], json!([]));
        assert_eq!(empty["ephemeral"], true);
        assert_eq!(empty["approvalPolicy"], "never");
        assert_eq!(empty["sandbox"], "read-only");
        assert_eq!(empty["environments"], json!([]));
        assert_eq!(empty["config"]["mcp_servers"], json!({}));
        assert_eq!(empty["config"]["features.shell_tool"], false);
        assert_eq!(empty["config"]["features.unified_exec"], false);
        assert_eq!(
            empty["config"],
            json!({
                "features.view_image": false,
                "features.shell_tool": false,
                "features.unified_exec": false,
                "features.multi_agent": false,
                "features.multi_agent_v2": false,
                "agents.enabled": false,
                "features.goals": false,
                "tools.experimental_request_user_input.enabled": false,
                "tools.update_plan.enabled": false,
                "features.code_mode.direct_only_tool_namespaces": ["web", "functions"],
                "web_search": "live",
                "mcp_servers": {},
                "skills.include_instructions": false,
            })
        );
        let tool = DynamicTool {
            name: "echo_test".into(),
            description: "Synthetic echo".into(),
            input_schema: json!({"type": "object"}),
        };
        let configured = serde_json::to_value(ThreadSettings::new(vec![tool.clone()]))?;
        assert_eq!(configured["dynamicTools"], json!([tool]));
        Ok(())
    }
}
