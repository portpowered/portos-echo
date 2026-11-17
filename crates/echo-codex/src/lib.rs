//! Settings for an isolated, client-tool-only pinned Codex app-server thread.
//! Transport and process supervision are not yet exposed by this crate.
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
                "features.shell_tool": false,
                "features.unified_exec": false,
                "features.multi_agent": false,
                "features.multi_agent_v2": false,
                "features.goals": false,
                "tools.experimental_request_user_input.enabled": false,
                "tools.update_plan.enabled": false,
                "web_search": "disabled",
                "mcp_servers": {},
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
