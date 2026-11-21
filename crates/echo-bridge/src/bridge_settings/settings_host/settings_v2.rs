//! V2 settings use the V1 store, prompt cache and restart receipt.
use super::{SettingsError, SettingsHost};
use crate::bridge_mcp::{
    Grant,
    bridge_operations::{Operation, StartError},
};
use crate::bridge_settings::portos_permissions::portos_server;
use echo_config::{BridgeConfig, SettingsOptions, SettingsOverlay, SettingsServerTools, WebSearch};
use echo_mcp_auth::GrantStatus;
use echo_screen_contract::{
    settings_v1::LinkStatusV1,
    settings_v2::{
        AgentPatchV2, AgentValuesV2, CodexValuesV2, HomeSettingsV2, HomeToolGroupV2, LinkStatusV2, PortosActionV2,
        PortosOperationStartedV2, PortosOperationStateV2, PortosOperationV2, PortosScopeV2, PortosServerV2,
        PortosStatusV2, PortosToolV2, SaveResultV2, SettingsErrorV2, SettingsFieldsV2, SettingsSnapshotV2,
        SettingsValuesV2, SettingsVersionV2, WebSearchV2,
    },
};
use echo_screen_server::SettingsReply;

impl SettingsHost {
    pub(super) fn portos_operation(&self, request: PortosOperationV2, grant: Option<&Grant>) -> SettingsReply {
        let config = self.store.effective();
        if portos_server(config).is_none() {
            return SettingsReply::InvalidV2(operation_error("Port OS is unconfigured."));
        }
        let Some(grant) = grant else {
            return SettingsReply::Unavailable;
        };
        let (operation, action) = match request {
            PortosOperationV2::Link { .. } => (Operation::Link, PortosActionV2::Link),
            PortosOperationV2::Unlink { .. } => (Operation::Unlink, PortosActionV2::Unlink),
        };
        if operation == Operation::Link && config.mcp.scopes != self.launch_scopes {
            return SettingsReply::AccountConflict(operation_error(
                "Port OS linking is waiting for the bridge restart.",
            ));
        }
        match grant.start_screen_operation(operation) {
            Ok(()) => SettingsReply::AccountStarted(PortosOperationStartedV2 {
                version: SettingsVersionV2,
                operation: action,
            }),
            Err(StartError::Busy) => portos_busy(),
            Err(StartError::Closed) => SettingsReply::Unavailable,
        }
    }

    pub(super) fn save_v2(&mut self, mut fields: SettingsFieldsV2) -> Result<SettingsReply, SettingsError> {
        if !fields.valid_shape() {
            return Err(SettingsError::Prompt("settings: expected an editable field"));
        }
        validate_legacy_inputs(fields.agent.as_ref())?;
        if legacy_only(&fields) {
            return Ok(self.saved_v2());
        }
        let muse = fields.muse.take();
        let patch = self.patch_v2(fields)?;
        self.restart_pending |= self.persist_muse(patch, muse)?;
        Ok(self.saved_v2())
    }

    fn patch_v2(&self, fields: SettingsFieldsV2) -> Result<SettingsOverlay, SettingsError> {
        let codex = fields.codex.unwrap_or_default();
        let agent = fields.agent.unwrap_or_default();
        let voice = fields.voice.unwrap_or_default();
        self.check_voice(codex.voice.as_deref())?;
        let daily_cap = agent
            .daily_request_cap
            .map(|value| value.as_str().parse::<u64>())
            .transpose()
            .map_err(|_| SettingsError::Prompt("agent.daily_request_cap: must fit a TOML integer"))?;
        let portos = fields.portos.unwrap_or_default();
        let server = portos
            .allow_tools
            .map(|tools| {
                let server = portos_server(self.store.effective())
                    .ok_or(SettingsError::Prompt("mcp.server: expected active Port OS server"))?;
                Ok::<_, SettingsError>(SettingsServerTools {
                    name: server.name.clone(),
                    allow_tools: tools.into_iter().map(|tool| tool.as_str().into()).collect(),
                })
            })
            .transpose()?;
        Ok(SettingsOverlay::from_leaves(
            voice.session_listen_enabled,
            voice.session_listen_seconds,
            codex.voice,
            daily_cap,
            None,
        )
        .with_options(SettingsOptions {
            web_search: codex.web_search.map(|mode| match mode {
                WebSearchV2::Live => WebSearch::Live,
                WebSearchV2::Cached => WebSearch::Cached,
                WebSearchV2::Disabled => WebSearch::Disabled,
            }),
            delegate_model: codex.delegate_model,
            delegate_effort: codex.delegate_effort,
            delegate_fast: codex.delegate_fast,
            agent_tools: fields.home.map(|home| {
                home.agent_tools
                    .into_iter()
                    .map(|group| group.as_str().into())
                    .collect()
            }),
            scopes: portos
                .scopes
                .map(|scopes| scopes.into_iter().map(|scope| scope.as_str().into()).collect()),
            server,
        }))
    }

    fn saved_v2(&self) -> SettingsReply {
        SettingsReply::SavedV2(SaveResultV2 {
            version: SettingsVersionV2,
            restart_pending: self.restart_pending,
        })
    }

    pub(super) fn snapshot_v2(
        &self,
        link: LinkStatusV1,
        grant: GrantStatus,
        include_muse: bool,
    ) -> Result<SettingsSnapshotV2, SettingsError> {
        let accepted = self.snapshot(link)?;
        let config = self.store.effective();
        let home = config.home.as_ref().map(|home| HomeSettingsV2 {
            agent_tools: [
                home.agent_tools.notes.then_some(HomeToolGroupV2::Notes),
                home.agent_tools.prompts.then_some(HomeToolGroupV2::Prompts),
            ]
            .into_iter()
            .flatten()
            .collect(),
        });
        Ok(SettingsSnapshotV2 {
            version: SettingsVersionV2,
            settings: SettingsValuesV2 {
                voice: accepted.settings.voice,
                codex: CodexValuesV2 {
                    voice: accepted.settings.codex.voice,
                    web_search: match config.codex.web_search {
                        WebSearch::Live => WebSearchV2::Live,
                        WebSearch::Cached => WebSearchV2::Cached,
                        WebSearch::Disabled => WebSearchV2::Disabled,
                    },
                    delegate_model: config.codex_model().unwrap_or_default().into(),
                    delegate_effort: config.codex.delegate_effort.clone().unwrap_or_default(),
                    delegate_fast: config.codex.delegate_fast,
                },
                agent: AgentValuesV2 {
                    daily_request_cap: accepted.settings.agent.daily_request_cap,
                },
                home,
            },
            allowed_voices: accepted.allowed_voices,
            prompts: accepted.prompts,
            portos: portos_status(config, &self.launch_scopes, grant),
            muse: include_muse.then(|| self.muse_status()),
            bridge_version: accepted.bridge_version,
            restart_pending: accepted.restart_pending,
            warnings: accepted.warnings,
        })
    }
}

pub(in crate::bridge_settings) fn legacy_only(fields: &SettingsFieldsV2) -> bool {
    fields.voice.is_none()
        && fields.codex.is_none()
        && fields.home.is_none()
        && fields.portos.is_none()
        && fields.muse.is_none()
        && fields.agent.as_ref().is_some_and(|agent| {
            agent.daily_request_cap.is_none()
                && (agent.max_tool_calls.is_some() || agent.task_deadline_seconds.is_some())
        })
}

fn validate_legacy_inputs(agent: Option<&AgentPatchV2>) -> Result<(), SettingsError> {
    if let Some(agent) = agent {
        if agent.max_tool_calls.is_some_and(|cap| !(1..=32).contains(&cap)) {
            return Err(SettingsError::Prompt("agent.max_tool_calls: must be within 1..=32"));
        }
        if agent
            .task_deadline_seconds
            .is_some_and(|seconds| !(5..=600).contains(&seconds))
        {
            return Err(SettingsError::Prompt(
                "agent.task_deadline_seconds: must be within 5..=600",
            ));
        }
    }
    Ok(())
}

pub(super) fn portos_busy() -> SettingsReply {
    SettingsReply::AccountConflict(operation_error("A Port OS operation is already in progress."))
}

fn operation_error(message: &str) -> SettingsErrorV2 {
    SettingsErrorV2 {
        version: SettingsVersionV2,
        message: message.into(),
    }
}

fn portos_status(config: &BridgeConfig, launch_scopes: &[String], grant: GrantStatus) -> PortosStatusV2 {
    let mut status = PortosStatusV2 {
        link_status: LinkStatusV2::Unconfigured,
        operation_state: PortosOperationStateV2::Idle,
        link_waiting_for_restart: false,
        relink_advised: false,
        scope_choices: PortosScopeV2::ALL.into(),
        tool_choices: PortosToolV2::ALL.into(),
        server: None,
        challenge: None,
    };
    if let Some(server) = portos_server(config) {
        status.link_status = match grant.scopes.as_ref() {
            None => LinkStatusV2::LinkRequired,
            Some(scopes) if config.mcp.scopes.iter().all(|scope| scopes.contains(scope)) => LinkStatusV2::Linked,
            Some(_) => LinkStatusV2::RelinkForScope,
        };
        status.link_waiting_for_restart = config.mcp.scopes != launch_scopes;
        status.relink_advised = grant.uses_legacy_cli;
        let url = server.url.split(['?', '#']).next().unwrap_or_default();
        status.server = Some(PortosServerV2 {
            name: server.name.clone(),
            url: url.into(),
            requested_scopes: config.mcp.scopes.clone(),
            granted_scopes: grant.scopes.unwrap_or_default(),
            allow_tools: server.allow_tools.clone(),
        });
    }
    status
}

pub(super) fn error_reply_v2(error: &SettingsError) -> SettingsReply {
    if matches!(error, SettingsError::RestartUnavailable) {
        return SettingsReply::Unavailable;
    }
    let body = SettingsErrorV2 {
        version: SettingsVersionV2,
        message: error.to_string(),
    };
    match error {
        SettingsError::Persistence | SettingsError::Durability | SettingsError::Unavailable => {
            SettingsReply::PersistenceV2(body)
        }
        _ => SettingsReply::InvalidV2(body),
    }
}
