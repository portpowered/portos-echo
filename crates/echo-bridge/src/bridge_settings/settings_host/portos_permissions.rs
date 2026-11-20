//! Running Port OS effects may only lose authority until runtime replacement.
use echo_config::{BridgeConfig, McpAuth, McpServer};
use echo_screen_contract::settings_v2::PortosToolV2;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub(crate) struct PortosPermissions(Arc<Mutex<Ceiling>>);

struct Ceiling {
    managed: bool,
    scopes: Vec<String>,
    tools: Vec<String>,
}

impl PortosPermissions {
    pub(crate) fn new(config: &BridgeConfig, managed: bool) -> Self {
        Self(Arc::new(Mutex::new(Ceiling {
            managed,
            scopes: config.mcp.scopes.clone(),
            tools: portos_server(config).map_or_else(Vec::new, |server| server.allow_tools.clone()),
        })))
    }

    pub(super) fn narrow(&self, config: &BridgeConfig) {
        if let Ok(mut ceiling) = self.0.lock() {
            ceiling.managed = true;
            ceiling.scopes.retain(|scope| config.mcp.scopes.contains(scope));
            ceiling
                .tools
                .retain(|tool| portos_server(config).is_some_and(|server| server.allow_tools.contains(tool)));
        }
    }

    pub(super) fn fail_closed(&self) {
        if let Ok(mut ceiling) = self.0.lock() {
            ceiling.managed = true;
            ceiling.tools.clear();
            ceiling.scopes.clear();
        }
    }

    pub(crate) fn managed(&self) -> bool {
        self.0.lock().map_or(true, |ceiling| ceiling.managed)
    }

    pub(crate) fn permits(&self, name: &str, grant: Option<&[String]>) -> bool {
        let Ok(ceiling) = self.0.lock() else { return false };
        if !ceiling.managed {
            return true;
        }
        let Some(grant) = grant else { return false };
        ceiling.tools.iter().any(|tool| tool == name)
            && PortosToolV2::ALL
                .into_iter()
                .find(|tool| tool.as_str() == name)
                .is_some_and(|tool| {
                    tool.scope().is_none_or(|scope| {
                        let scope = scope.as_str();
                        ceiling.scopes.iter().any(|configured| configured == scope)
                            && grant.iter().any(|granted| granted == scope)
                    })
                })
    }
}

pub(super) fn portos_server(config: &BridgeConfig) -> Option<&McpServer> {
    config
        .mcp
        .server
        .first()
        .filter(|server| server.auth == McpAuth::PortosDeviceCode)
}
