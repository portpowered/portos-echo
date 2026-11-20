//! Serialized, fixed-target persistence for the paired screen's settings host.
//! The host owns authorization and restart admission; this owner performs no exec.
use echo_config::{BridgeConfig, ConfigError, SettingsOverlay};
use std::path::PathBuf;

#[path = "settings_host/portos_permissions.rs"]
pub(crate) mod portos_permissions;
mod settings_atomic;
mod settings_host;
pub mod settings_process;
pub mod settings_restart;
mod settings_service;
pub use settings_host::SettingsHost;
pub use settings_service::{SettingsCompletion, SettingsService};

pub(crate) const RESTART_FAILED: &str = "Could not restart bridge; saved settings remain pending";

/// Safe diagnostics: no submitted prompt, setting value or filesystem path.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("home.dir: settings unavailable")]
    Unavailable,
    #[error("{0}")]
    Validation(#[from] ConfigError),
    #[error("{0}")]
    Prompt(&'static str),
    #[error("Could not save file; previous file retained")]
    Persistence,
    #[error("File replaced but durability is uncertain; reload before saving again")]
    Durability,
}

/// The only two prompt destinations. HTTP path strings never become file paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    Voice,
    Runner,
}

impl PromptKind {
    fn filename(self) -> &'static str {
        match self {
            Self::Voice => "voice.md",
            Self::Runner => "runner.md",
        }
    }

    fn validate(self, text: &str) -> Result<(), SettingsError> {
        let message = match (self, text.trim().is_empty(), text.len() > 32_768) {
            (Self::Voice, true, _) => Some("prompts/voice.md: empty prompt"),
            (Self::Runner, true, _) => Some("prompts/runner.md: empty prompt"),
            (Self::Voice, _, true) => Some("prompts/voice.md: prompt exceeds 32768 bytes"),
            (Self::Runner, _, true) => Some("prompts/runner.md: prompt exceeds 32768 bytes"),
            _ => None,
        };
        message.map_or(Ok(()), |message| Err(SettingsError::Prompt(message)))
    }
}

/// One mutable owner serializes sparse merges and atomic replacements.
/// `base` is the validated operator configuration, never the overlaid config.
/// The active runtime keeps its own config until a separately admitted restart.
pub struct SettingsStore {
    base: BridgeConfig,
    overlay: SettingsOverlay,
    effective: BridgeConfig,
    home: PathBuf,
    uncertain: bool,
}

impl SettingsStore {
    /// Mount only a configured home and an already accepted startup overlay.
    ///
    /// # Errors
    /// Refuses absent home, invalid candidate or unavailable directory.
    pub fn open(base: BridgeConfig, overlay: SettingsOverlay) -> Result<Self, SettingsError> {
        let effective = overlay.apply_to(&base)?;
        let home = base.home.as_ref().ok_or(SettingsError::Unavailable)?.dir.clone();
        if !home.is_absolute() {
            return Err(SettingsError::Unavailable);
        }
        crate::bridge_private::directory(&home).map_err(|_| SettingsError::Unavailable)?;
        Ok(Self {
            base,
            overlay,
            effective,
            home,
            uncertain: false,
        })
    }

    /// Last durably accepted editable values, distinct from the running config.
    #[must_use]
    pub fn effective(&self) -> &BridgeConfig {
        &self.effective
    }

    /// Merge and validate before touching disk. Returns whether runtime values
    /// changed from the last accepted save; an effective no-op needs no restart.
    ///
    /// # Errors
    /// Returns echo-config's validation error or a fixed persistence diagnostic.
    pub fn save(&mut self, patch: &SettingsOverlay) -> Result<bool, SettingsError> {
        self.save_with(patch, |_| Ok(()))
    }

    fn save_with(
        &mut self,
        patch: &SettingsOverlay,
        check: impl FnMut(settings_atomic::Stage) -> std::io::Result<()>,
    ) -> Result<bool, SettingsError> {
        if self.uncertain {
            return Err(SettingsError::Durability);
        }
        let candidate = self.overlay.merge(patch);
        let config = candidate.apply_to(&self.base)?;
        let changed = !same_values(&self.effective, &config);
        if candidate != self.overlay {
            let text = candidate.to_toml_string()?;
            let result = settings_atomic::replace(&self.home, "settings.toml", text.as_bytes(), check);
            self.uncertain = matches!(result, Err(SettingsError::Durability));
            result?;
            self.overlay = candidate;
            self.effective = config;
        }
        Ok(changed)
    }

    /// Save exact UTF-8 bytes to a fixed prompt destination, without a restart.
    ///
    /// # Errors
    /// Refuses blank/oversized text and reports persistence failure safely.
    pub fn save_prompt(&mut self, kind: PromptKind, text: &str) -> Result<(), SettingsError> {
        kind.validate(text)?;
        let directory = self.home.join("prompts");
        crate::bridge_private::directory(&directory).map_err(|_| SettingsError::Persistence)?;
        settings_atomic::replace(&directory, kind.filename(), text.as_bytes(), |_| Ok(()))
    }
}

fn same_values(first: &BridgeConfig, second: &BridgeConfig) -> bool {
    first.voice.session_listen_enabled == second.voice.session_listen_enabled
        && first.voice.session_listen_seconds == second.voice.session_listen_seconds
        && first.codex_voice() == second.codex_voice()
        && first.agent.daily_request_cap == second.agent.daily_request_cap
        && first.agent.max_tool_calls == second.agent.max_tool_calls
        && first.codex.web_search == second.codex.web_search
        && first.codex.delegate_model == second.codex.delegate_model
        && first.codex.delegate_effort == second.codex.delegate_effort
        && first.agent.task_deadline_seconds == second.agent.task_deadline_seconds
        && first.home == second.home
        && first.mcp == second.mcp
}

#[cfg(test)]
mod settings_tests;

#[cfg(test)]
pub(crate) mod settings_http_tests;
