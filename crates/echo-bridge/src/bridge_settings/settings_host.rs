//! Typed transactions for the bridge's single settings owner. No task or exec here.
use super::{PromptKind, SettingsError, SettingsStore, portos_permissions::PortosPermissions};
use crate::bridge_mcp::Grant;
#[path = "settings_host/settings_muse.rs"]
mod settings_muse;
#[path = "settings_host/settings_v2.rs"]
pub(super) mod settings_v2;
use echo_config::{BridgeConfig, SettingsOverlay};
use echo_mcp_auth::GrantStatus;
use echo_screen_contract::settings_v1::{
    AgentValuesV1, CodexPatchV1, DailyCapV1, LinkStatusV1, PromptTextsV1, SaveResultV1, SettingsErrorV1,
    SettingsFieldsV1, SettingsSnapshotV1, SettingsValuesV1, SettingsVersionV1, VoiceValuesV1,
};
use echo_screen_server::{SettingsOperation, SettingsReply};
use settings_v2::error_reply_v2;

/// Serialized by the runtime's inbox. Construct with the original validated base,
/// accepted startup overlay and admitted prompt texts, never the effective base.
pub struct SettingsHost {
    store: SettingsStore,
    prompts: PromptTextsV1,
    warnings: Vec<String>,
    restart_pending: bool,
    launch_scopes: Vec<String>,
    permissions: PortosPermissions,
    muse: settings_muse::MuseSettings,
    /// The realtime voices the running app server listed; `None` until it has.
    voices: Option<Vec<String>>,
}

impl SettingsHost {
    pub(crate) fn prompts(&self) -> PromptTextsV1 {
        self.prompts.clone()
    }

    /// Retain pending edits and report a failed replacement without retrying it.
    pub(crate) fn replacement_failed(&mut self) {
        if !self.warnings.iter().any(|warning| warning == super::RESTART_FAILED) {
            self.warnings.push(super::RESTART_FAILED.into());
        }
    }

    /// # Errors
    /// Refuses unavailable home, invalid overlay or inadmissible prompt cache.
    pub fn open(
        base: BridgeConfig,
        overlay: SettingsOverlay,
        prompts: PromptTextsV1,
        warnings: Vec<String>,
    ) -> Result<Self, SettingsError> {
        PromptKind::Voice.validate(&prompts.voice)?;
        PromptKind::Runner.validate(&prompts.runner)?;
        let store = SettingsStore::open(base, overlay)?;
        let launch_scopes = store.effective().mcp.scopes.clone();
        let permissions = PortosPermissions::new(store.effective(), store.overlay.manages_portos());
        Ok(Self {
            store,
            launch_scopes,
            permissions,
            muse: settings_muse::MuseSettings::default(),
            prompts,
            warnings,
            restart_pending: false,
            voices: None,
        })
    }

    /// Adopt the list the admitted app server reported. A later generation that
    /// could not list keeps the last one.
    pub(super) fn listed_voices(&mut self, voices: Option<Vec<String>>) {
        if voices.is_some() {
            self.voices = voices;
        }
    }

    /// `default`, then every voice the running app server lists. Before it has
    /// listed any, echo-config's built-in names. The saved voice is always
    /// present, so a snapshot can show a choice a newer release dropped.
    fn allowed_voices(&self, saved: Option<&str>) -> Vec<String> {
        let mut names: Vec<String> = match &self.voices {
            Some(listed) => std::iter::once("default")
                .chain(listed.iter().map(String::as_str))
                .map(String::from)
                .collect(),
            None => echo_config::realtime_voices().map(String::from).collect(),
        };
        if let Some(saved) = saved
            && !names.iter().any(|name| name == saved)
        {
            names.push(saved.into());
        }
        names
    }

    /// A voice the running app server does not list would be left out of every
    /// realtime start, so it is refused at save time once the list is known.
    pub(super) fn check_voice(&self, voice: Option<&str>) -> Result<(), SettingsError> {
        match (voice, &self.voices) {
            (Some(voice), Some(listed)) if voice != "default" && !listed.iter().any(|name| name == voice) => Err(
                SettingsError::Prompt("codex.voice: not listed by the app server; choose default or a listed voice"),
            ),
            _ => Ok(()),
        }
    }

    pub(super) fn permissions(&self) -> PortosPermissions {
        self.permissions.clone()
    }

    pub(super) fn disable_restart(&mut self) {
        self.store.restart_allowed = false;
    }

    pub(super) fn muse_access(&self) -> crate::bridge_muse::Access {
        self.muse.access.clone()
    }

    pub(super) fn restrict_muse_to_loopback(&mut self) {
        self.muse.local = true;
    }

    #[cfg(test)]
    pub(super) fn fail_next_overlay_directory_sync(&mut self) {
        self.store.fail_next_directory_sync = true;
    }

    /// Call only inside `SettingsRequest::commit`; no detached work or retries.
    /// The returned pending flag records a request, not proof of an actual exec.
    pub fn transaction(&mut self, operation: SettingsOperation, link: LinkStatusV1) -> SettingsReply {
        self.transaction_with_grant(operation, link, GrantStatus::default(), None)
    }

    pub(super) fn transaction_with_grant(
        &mut self,
        operation: SettingsOperation,
        link: LinkStatusV1,
        grant: GrantStatus,
        owner: Option<&Grant>,
    ) -> SettingsReply {
        let result = match operation {
            SettingsOperation::PackCatalog(catalog) | SettingsOperation::PackWatch(catalog, _) => Ok(
                SettingsReply::Packs(catalog.with_selection(self.store.overlay.screen_pack())),
            ),
            SettingsOperation::PackRefresh(catalog, snapshot) => self.refresh_pack(catalog, &snapshot),
            SettingsOperation::PackSelection(catalog, selection) => self.select_pack(catalog, &selection),
            SettingsOperation::Snapshot => self.snapshot(link).map(SettingsReply::Snapshot),
            SettingsOperation::SnapshotV2 { include_muse } => {
                return self.snapshot_v2(link, grant, include_muse).map_or_else(
                    |error| error_reply_v2(&error),
                    |snapshot| SettingsReply::SnapshotV2(Box::new(snapshot)),
                );
            }
            SettingsOperation::SaveV2(patch) => {
                if patch.settings.portos.is_some() && owner.is_some_and(Grant::screen_operation_active) {
                    return settings_v2::portos_busy();
                }
                return self
                    .save_v2(patch.settings)
                    .unwrap_or_else(|error| error_reply_v2(&error));
            }
            SettingsOperation::PortosV2(request) => return self.portos_operation(request, owner),
            SettingsOperation::OpenAiSnapshot
            | SettingsOperation::OpenAiOperation(_)
            | SettingsOperation::MuseProbe => {
                return SettingsReply::Unavailable;
            }
            SettingsOperation::Save(patch) => self.save(patch.settings).map(saved),
            SettingsOperation::Prompt(kind, prompt) => {
                let kind = match kind {
                    echo_screen_server::PromptKind::Voice => PromptKind::Voice,
                    echo_screen_server::PromptKind::Runner => PromptKind::Runner,
                };
                self.store.save_prompt(kind, &prompt.text).map(|()| {
                    match kind {
                        PromptKind::Voice => self.prompts.voice = prompt.text,
                        PromptKind::Runner => self.prompts.runner = prompt.text,
                    }
                    saved(false)
                })
            }
        };
        result.unwrap_or_else(|error| error_reply(&error))
    }

    fn select_pack(
        &mut self,
        catalog: echo_screen_server::PackCatalog,
        selection: &str,
    ) -> Result<SettingsReply, SettingsError> {
        if !catalog.accepts(selection) {
            return Ok(SettingsReply::InvalidPackSelection);
        }
        let catalog = catalog.with_selection(self.store.overlay.screen_pack());
        let changed = catalog.selection_changes(selection);
        let patch = SettingsOverlay::from_toml_str(&format!("screen_pack='{selection}'"))?;
        self.persist(&patch)?;
        Ok(SettingsReply::PackSaved(
            save_result(self.restart_pending),
            catalog.with_selection(Some(selection)),
            changed,
        ))
    }

    fn refresh_pack(
        &mut self,
        catalog: echo_screen_server::PackCatalog,
        snapshot: &echo_screen_server::PackSettings,
    ) -> Result<SettingsReply, SettingsError> {
        let overlay = SettingsOverlay::from_toml_str(snapshot.text())?;
        let effective = overlay.apply_to(&self.store.base)?;
        if !snapshot.is_current(&self.store.home) {
            return Err(SettingsError::Unavailable);
        }
        let catalog = catalog.with_selection(self.store.overlay.screen_pack());
        let changed = catalog.selection_changes(overlay.screen_pack().unwrap_or(echo_screen_server::DEFAULT_PACK));
        // Adopt pending editable values without changing the running config,
        // writing settings, requesting exec or clearing durability uncertainty.
        if overlay.manages_portos() {
            self.permissions.narrow(&effective);
        }
        if effective.muse != self.store.effective.muse {
            self.muse.observation = None;
            if effective.muse.is_none() {
                self.muse.access.withdraw();
            }
        }
        self.store.overlay = overlay;
        self.store.effective = effective;
        Ok(SettingsReply::PackRefreshed(
            catalog.with_selection(self.store.overlay.screen_pack()),
            changed,
        ))
    }

    fn save(&mut self, fields: SettingsFieldsV1) -> Result<bool, SettingsError> {
        if !fields.is_nonempty() {
            return Err(SettingsError::Prompt("settings: expected an editable field"));
        }
        let voice = fields.voice.unwrap_or_default();
        let agent = fields.agent.unwrap_or_default();
        self.check_voice(fields.codex.as_ref().map(|codex| codex.voice.as_str()))?;
        let daily_cap = agent
            .daily_request_cap
            .map(|value| value.as_str().parse::<u64>())
            .transpose()
            .map_err(|_| SettingsError::Prompt("agent.daily_request_cap: must fit a TOML integer"))?;
        let patch = SettingsOverlay::from_leaves(
            voice.session_listen_enabled,
            voice.session_listen_seconds,
            fields.codex.map(|value| value.voice),
            daily_cap,
            agent.max_tool_calls,
        );
        self.restart_pending |= self.persist(&patch)?;
        Ok(self.restart_pending)
    }

    fn persist(&mut self, patch: &SettingsOverlay) -> Result<bool, SettingsError> {
        self.persist_with(patch, SettingsStore::save)
    }

    fn persist_with(
        &mut self,
        patch: &SettingsOverlay,
        write: impl FnOnce(&mut SettingsStore, &SettingsOverlay) -> Result<bool, SettingsError>,
    ) -> Result<bool, SettingsError> {
        let saved = write(&mut self.store, patch);
        if matches!(saved, Err(SettingsError::Durability)) {
            self.permissions.fail_closed();
        } else if saved.is_ok() && patch.manages_portos() {
            self.permissions.narrow(self.store.effective());
        }
        saved
    }

    fn snapshot(&self, link_status: LinkStatusV1) -> Result<SettingsSnapshotV1, SettingsError> {
        let config = self.store.effective();
        let daily_request_cap = DailyCapV1::try_from(config.agent.daily_request_cap.to_string())
            .map_err(|_| SettingsError::Prompt("agent.daily_request_cap: cannot represent value"))?;
        Ok(SettingsSnapshotV1 {
            version: SettingsVersionV1,
            settings: SettingsValuesV1 {
                voice: VoiceValuesV1 {
                    session_listen_enabled: config.voice.session_listen_enabled,
                    session_listen_seconds: config.voice.session_listen_seconds,
                },
                codex: CodexPatchV1 {
                    voice: config.codex_voice().unwrap_or("default").into(),
                },
                agent: AgentValuesV1 {
                    daily_request_cap,
                    max_tool_calls: self.store.overlay.legacy_tool_cap().unwrap_or(8),
                },
            },
            allowed_voices: self.allowed_voices(config.codex_voice()),
            prompts: self.prompts.clone(),
            link_status,
            bridge_version: env!("CARGO_PKG_VERSION").into(),
            restart_pending: self.restart_pending,
            warnings: self.warnings.clone(),
        })
    }
}

fn saved(restart_pending: bool) -> SettingsReply {
    SettingsReply::Saved(save_result(restart_pending))
}

fn save_result(restart_pending: bool) -> SaveResultV1 {
    SaveResultV1 {
        version: SettingsVersionV1,
        restart_pending,
    }
}

fn error_reply(error: &SettingsError) -> SettingsReply {
    if matches!(error, SettingsError::RestartUnavailable) {
        return SettingsReply::Unavailable;
    }
    let persistence = matches!(
        error,
        SettingsError::Persistence | SettingsError::Durability | SettingsError::Unavailable
    );
    let error = SettingsErrorV1 {
        version: SettingsVersionV1,
        message: error.to_string(),
    };
    if persistence {
        SettingsReply::Persistence(error)
    } else {
        SettingsReply::Invalid(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge_settings::settings_atomic::Stage;
    use echo_config::{AgentTools, HomeConfig, McpAuth, McpServer};
    use std::io::Write as _;

    /// No Codex release is pinned: the picker is the running app server's list.
    #[test]
    fn screen_settings_voices_follow_the_running_app_server() {
        let root = tempfile::tempdir().unwrap();
        let mut base =
            BridgeConfig::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/bridge.echo.toml")).unwrap();
        base.home = Some(HomeConfig {
            dir: root.path().join("home"),
            agent_tools: AgentTools::default(),
        });
        let prompts = PromptTextsV1 {
            voice: "Synthetic voice".into(),
            runner: "Synthetic runner".into(),
        };
        let mut host = SettingsHost::open(base, SettingsOverlay::default(), prompts, Vec::new()).unwrap();
        let offered = |host: &SettingsHost| host.snapshot(LinkStatusV1::Unconfigured).unwrap().allowed_voices;
        let save = |host: &mut SettingsHost, voice: &str| {
            host.save(SettingsFieldsV1 {
                codex: Some(CodexPatchV1 { voice: voice.into() }),
                ..SettingsFieldsV1::default()
            })
            .map_err(|error| error.to_string())
        };
        // Before the app server has answered: the built-in names.
        assert_eq!(offered(&host), echo_config::realtime_voices().collect::<Vec<_>>());
        host.listed_voices(Some(
            ["cove", "miko", "himari", "hanul", "leher"].map(String::from).to_vec(),
        ));
        // A generation that could not list keeps the last list.
        host.listed_voices(None);
        assert_eq!(offered(&host), ["default", "cove", "miko", "himari", "hanul", "leher"]);
        for voice in ["miko", "himari", "hanul", "leher", "default"] {
            assert_eq!(save(&mut host, voice), Ok(true), "{voice}");
        }
        assert_eq!(save(&mut host, "himari"), Ok(true));
        assert_eq!(
            host.snapshot(LinkStatusV1::Unconfigured).unwrap().settings.codex.voice,
            "himari"
        );
        // `maple` is a built-in name this app server does not list; `alloy` is v2-only.
        for (voice, reason) in [
            (
                "maple",
                "codex.voice: not listed by the app server; choose default or a listed voice",
            ),
            (
                "alloy",
                "codex.voice: not listed by the app server; choose default or a listed voice",
            ),
        ] {
            assert_eq!(save(&mut host, voice), Err(reason.into()));
        }
        // A release that drops the saved voice still shows it, last.
        host.listed_voices(Some(vec!["cove".into()]));
        assert_eq!(offered(&host), ["default", "cove", "himari"]);
    }

    #[test]
    fn screen_settings_store_uncertain_portos_save_denies_pending_effects() {
        let root = tempfile::tempdir().unwrap();
        let mut base =
            BridgeConfig::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/bridge.echo.toml")).unwrap();
        base.home = Some(HomeConfig {
            dir: root.path().join("home"),
            agent_tools: AgentTools::default(),
        });
        base.mcp.server[0] = McpServer {
            name: "portos".into(),
            url: "https://127.0.0.1/mcp".into(),
            auth: McpAuth::PortosDeviceCode,
            bearer_file: None,
            allow_tools: vec!["control".into(), "endpoint_query".into()],
        };
        let mut host = SettingsHost::open(
            base,
            SettingsOverlay::default(),
            PromptTextsV1 {
                voice: "Synthetic voice".into(),
                runner: "Synthetic runner".into(),
            },
            Vec::new(),
        )
        .unwrap();
        let grant = ["endpoint:read".into(), "message:send".into()];
        assert!(host.permissions().permits("endpoint_query", Some(&grant)));
        let patch = SettingsOverlay::from_toml_str("[mcp]\nscopes=['endpoint:read']").unwrap();
        let err = host
            .persist_with(&patch, |store, patch| {
                store.save_with(patch, |stage| {
                    if stage == Stage::DirectorySync {
                        Err(std::io::Error::other("injected sync failure"))
                    } else {
                        Ok(())
                    }
                })
            })
            .unwrap_err();
        let SettingsReply::PersistenceV2(body) = error_reply_v2(&err) else {
            unreachable!("expected V2 persistence reply");
        };
        assert_eq!(
            body.message,
            "File replaced but durability is uncertain; reload before saving again"
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("home/settings.toml")).unwrap(),
            "[mcp]\nscopes = [\"endpoint:read\"]\n"
        );
        assert!(!host.restart_pending);
        assert!(host.permissions().managed());
        assert!(!host.permissions().permits("endpoint_query", Some(&grant)));
        assert!(!host.permissions().permits("control", Some(&grant)));
        writeln!(
            std::io::stdout().lock(),
            "{}; control and endpoint_query denied",
            body.message
        )
        .unwrap();
    }
}
