//! Typed transactions for the bridge's single settings owner. No task or exec here.
use super::{PromptKind, SettingsError, SettingsStore};
use echo_config::{BridgeConfig, SettingsOverlay};
use echo_screen_contract::settings_v1::{
    AgentValuesV1, CodexPatchV1, DailyCapV1, LinkStatusV1, PromptTextsV1, SaveResultV1, SettingsErrorV1,
    SettingsFieldsV1, SettingsSnapshotV1, SettingsValuesV1, SettingsVersionV1, VoiceValuesV1,
};
use echo_screen_server::{SettingsOperation, SettingsReply};

/// Serialized by the runtime's inbox. Construct with the original validated base,
/// accepted startup overlay and admitted prompt texts, never the effective base.
pub struct SettingsHost {
    store: SettingsStore,
    prompts: PromptTextsV1,
    warnings: Vec<String>,
    restart_pending: bool,
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
        Ok(Self {
            store: SettingsStore::open(base, overlay)?,
            prompts,
            warnings,
            restart_pending: false,
        })
    }

    /// Call only inside `SettingsRequest::commit`; no detached work or retries.
    /// The returned pending flag records a request, not proof of an actual exec.
    pub fn transaction(&mut self, operation: SettingsOperation, link: LinkStatusV1) -> SettingsReply {
        let result = match operation {
            SettingsOperation::PackCatalog(catalog) => Ok(SettingsReply::Packs(
                catalog.with_selection(self.store.overlay.screen_pack()),
            )),
            SettingsOperation::PackRefresh(catalog, snapshot) => self.refresh_pack(catalog, &snapshot),
            SettingsOperation::PackSelection(catalog, selection) => {
                if !catalog.accepts(&selection) {
                    return SettingsReply::InvalidPackSelection;
                }
                let catalog = catalog.with_selection(self.store.overlay.screen_pack());
                let changed = catalog.selection_changes(&selection);
                SettingsOverlay::from_toml_str(&format!("screen_pack='{selection}'"))
                    .map_err(SettingsError::from)
                    .and_then(|patch| self.store.save(&patch))
                    .map(|_| {
                        SettingsReply::PackSaved(
                            save_result(self.restart_pending),
                            catalog.with_selection(Some(&selection)),
                            changed,
                        )
                    })
            }
            SettingsOperation::Snapshot => self.snapshot(link).map(SettingsReply::Snapshot),
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
        let changed = catalog.selection_changes(overlay.screen_pack().unwrap_or("template"));
        // Adopt pending editable values without changing the running config,
        // writing settings, requesting exec or clearing durability uncertainty.
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
        self.restart_pending |= self.store.save(&patch)?;
        Ok(self.restart_pending)
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
                    max_tool_calls: config.agent.max_tool_calls,
                },
            },
            allowed_voices: echo_config::realtime_voices().map(String::from).collect(),
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
