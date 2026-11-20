//! Explicit local-fake routing, passed only to the owned child process.
use crate::{Error, bridge_input::lab_origin};
use echo_codex::codex_supervisor::{Supervisor, SupervisorConfig};
use serde_json::json;

/// Validated numeric loopback origins. Payloads are never diagnostics.
pub struct LocalRoutes {
    live: String,
    model: String,
}

impl LocalRoutes {
    /// # Errors
    /// Reject non-loopback origins, credentials, paths and zero ports offline.
    pub fn new(live: &str, model: &str) -> Result<Self, Error> {
        Ok(Self {
            live: lab_origin(live)?.trim_end_matches('/').into(),
            model: lab_origin(model)?.trim_end_matches('/').into(),
        })
    }

    pub(super) fn configure(&self, config: &mut SupervisorConfig, web_search: echo_config::WebSearch) {
        let values = [
            ("model_provider", json!("local")),
            (
                "model_providers.local",
                json!({"name":"local", "base_url":format!("{}/v1", self.model),
                "wire_api":"responses", "requires_openai_auth":true,
                "supports_standalone_web_search":true,
                "experimental_bearer_token":"dummy-local-only"}),
            ),
            (
                "experimental_realtime_webrtc_call_base_url",
                json!(format!("{}/backend-api/codex", self.live)),
            ),
            ("experimental_realtime_ws_base_url", json!(format!("{}/v1", self.live))),
        ];
        for (key, value) in values {
            // These values are strings or a single flat inline TOML table.
            let value = match value.as_object() {
                Some(table) => format!(
                    "{{{}}}",
                    table
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                None => value.to_string(),
            };
            config.config_overrides.push((key.into(), value));
        }
        restrictions(config, web_search);
    }

    pub(super) fn environment(&self, supervisor: &mut Supervisor) -> Result<(), Error> {
        supervisor
            .set_environment("CODEX_REFRESH_TOKEN_URL_OVERRIDE", format!("{}/token", self.live))
            .map_err(super::codex_host::process_error)?;
        // The fake also rejects proxy attempts to external origins. Set both
        // spellings because the pin's HTTP and websocket clients differ.
        for key in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            supervisor
                .set_environment(key, self.live.clone())
                .map_err(super::codex_host::process_error)?;
        }
        for key in ["NO_PROXY", "no_proxy"] {
            supervisor
                .set_environment(key, "127.0.0.1".into())
                .map_err(super::codex_host::process_error)?;
        }
        Ok(())
    }
}

fn restrictions(config: &mut SupervisorConfig, web_search: echo_config::WebSearch) {
    for key in [
        "features.plugins",
        "features.recommended_plugins",
        "features.remote_models",
        "features.apps",
        "features.remote_control",
        "features.skill_search",
        "cloud.skills.enabled",
        "analytics.enabled",
        "feedback.enabled",
        "features.multi_agent",
        "features.multi_agent_v2",
        "features.goals",
        "tools.experimental_request_user_input.enabled",
        "features.shell_tool",
        "features.unified_exec",
        "tools.update_plan.enabled",
        "check_for_update_on_startup",
    ] {
        config.config_overrides.push((key.into(), "false".into()));
    }
    for (key, value) in [
        ("features.skip_host_skill_discovery", "true"),
        ("features.code_mode.direct_only_tool_namespaces", "[\"web\"]"),
        ("web_search", &format!("\"{}\"", web_search.as_str())),
        ("mcp_servers", "{}"),
        ("cli_auth_credentials_store", "\"file\""),
    ] {
        config.config_overrides.push((key.into(), value.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_search_overrides_preserve_complete_local_restrictions() {
        for mode in [
            echo_config::WebSearch::Live,
            echo_config::WebSearch::Cached,
            echo_config::WebSearch::Disabled,
        ] {
            let mut config = SupervisorConfig::new("/unused/binary".into(), "/unused/home".into());
            restrictions(&mut config, mode);
            let mut expected = std::collections::BTreeMap::from([
                ("features.skip_host_skill_discovery".to_owned(), "true".to_owned()),
                ("features.code_mode.direct_only_tool_namespaces".into(), "[\"web\"]".into()),
                ("web_search".into(), format!("\"{}\"", mode.as_str())),
                ("mcp_servers".into(), "{}".into()),
                ("cli_auth_credentials_store".into(), "\"file\"".into()),
            ]);
            for key in [
                "features.plugins",
                "features.recommended_plugins",
                "features.remote_models",
                "features.apps",
                "features.remote_control",
                "features.skill_search",
                "cloud.skills.enabled",
                "analytics.enabled",
                "feedback.enabled",
                "features.multi_agent",
                "features.multi_agent_v2",
                "features.goals",
                "tools.experimental_request_user_input.enabled",
                "features.shell_tool",
                "features.unified_exec",
                "tools.update_plan.enabled",
                "check_for_update_on_startup",
            ] {
                expected.insert(key.into(), "false".into());
            }
            let observed = config
                .config_overrides
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>();
            assert_eq!(observed, expected);
        }
    }

    #[test]
    fn routes_reject_ambiguous_or_external_targets_before_runtime() {
        for invalid in [
            "https://127.0.0.1:7",
            "http://localhost:7",
            "http://127.0.0.1:0",
            "http://127.0.0.2:7", // lint-rules: allow-literal inert rejection input; never dialed
            "http://127.0.0.1:7/path",
            "http://user@127.0.0.1:7",
            "http://127.0.0.1:7?query",
            "http://127.0.0.1:7\n",
        ] {
            assert!(matches!(
                LocalRoutes::new(invalid, "http://127.0.0.1:8"),
                Err(Error::Input)
            ));
            assert!(matches!(
                LocalRoutes::new("http://127.0.0.1:8", invalid),
                Err(Error::Input)
            ));
        }
    }
}
