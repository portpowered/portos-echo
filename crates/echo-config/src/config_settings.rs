//! The screen's sparse, non-secret overlay. Never rewrites the operator's base.
use crate::{BridgeConfig, ConfigError, config_decode::Fields};
use std::{collections::BTreeMap, io::Read, path::Path};

const MAX_OVERLAY_BYTES: u64 = 65_536;

/// An allowlisted overlay, distinct from a complete bridge configuration.
/// Private fields ensure decoding and merges cannot introduce another key.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SettingsOverlay {
    screen_pack: Option<String>,
    listen_enabled: Option<bool>,
    listen_seconds: Option<u32>,
    voice: Option<String>,
    daily_cap: Option<u64>,
    tool_cap: Option<u32>,
}

/// Effective startup configuration with an optional safe, reportable warning.
/// A rejected overlay is ignored in its entirety, retaining the validated base.
#[derive(Debug)]
pub struct EffectiveConfig {
    /// Original validated operator config, retained for future sparse merges.
    pub base: BridgeConfig,
    pub config: BridgeConfig,
    pub overlay: SettingsOverlay,
    pub warning: Option<ConfigError>,
}

impl SettingsOverlay {
    /// Construct a sparse patch from typed leaves, without applying base defaults.
    /// The caller must use `apply_to` before accepting or persisting the candidate.
    #[must_use]
    pub fn from_leaves(
        listen_enabled: Option<bool>,
        listen_seconds: Option<u32>,
        voice: Option<String>,
        daily_cap: Option<u64>,
        tool_cap: Option<u32>,
    ) -> Self {
        Self {
            screen_pack: None,
            listen_enabled,
            listen_seconds,
            voice,
            daily_cap,
            tool_cap,
        }
    }

    /// Decode only editable keys. Validation against the base is separate.
    ///
    /// # Errors
    /// Returns fixed field diagnostics, never submitted keys, values or paths.
    pub fn from_toml_str(input: &str) -> Result<Self, ConfigError> {
        let table =
            toml::de::DeTable::parse(input).map_err(|_| ConfigError::new("settings.toml", "invalid TOML syntax"))?;
        let value = toml::de::DeValue::Table(table.into_inner());
        let mut fields = Fields::new(value, "config")?;
        let mut result = Self {
            screen_pack: fields.optional("screen_pack")?,
            ..Self::default()
        };
        if let Some(value) = result.screen_pack() {
            crate::config_screen::selection(value)?;
        }
        if let Some(mut voice) = fields.section("voice")? {
            result.listen_enabled = voice.optional("session_listen_enabled")?;
            result.listen_seconds = voice.optional("session_listen_seconds")?;
            voice.finish()?;
        }
        if let Some(mut codex) = fields.section("codex")? {
            result.voice = codex.optional("voice")?;
            codex.finish()?;
        }
        if let Some(mut agent) = fields.section("agent")? {
            result.daily_cap = agent.optional("daily_request_cap")?;
            result.tool_cap = agent.optional("max_tool_calls")?;
            agent.finish()?;
        }
        fields.finish()?;
        if let Some(value) = result.daily_cap {
            i64::try_from(value).map_err(|_| ConfigError::new("agent.daily_request_cap", "must fit a TOML integer"))?;
        }
        Ok(result)
    }

    /// Apply to a clone and validate the entire candidate, including cross-fields.
    ///
    /// # Errors
    /// Returns echo-config's safe validation diagnostic; the base is unchanged.
    pub fn apply_to(&self, base: &BridgeConfig) -> Result<BridgeConfig, ConfigError> {
        if let Some(value) = self.screen_pack() {
            crate::config_screen::selection(value)?;
        }
        let mut config = base.clone();
        if let Some(value) = self.listen_enabled {
            config.voice.session_listen_enabled = value;
        }
        if let Some(value) = self.listen_seconds {
            config.voice.session_listen_seconds = value;
        }
        if let Some(value) = &self.voice {
            config.codex.voice = Some(value.clone());
        }
        if let Some(value) = self.daily_cap {
            i64::try_from(value).map_err(|_| ConfigError::new("agent.daily_request_cap", "must fit a TOML integer"))?;
            config.agent.daily_request_cap = value;
        }
        if let Some(value) = self.tool_cap {
            config.agent.max_tool_calls = value;
        }
        crate::config_validate::validate(&config)?;
        Ok(config)
    }

    /// Merge a sparse patch without materializing base defaults or losing siblings.
    #[must_use]
    pub fn merge(&self, patch: &Self) -> Self {
        Self {
            screen_pack: patch.screen_pack.clone().or_else(|| self.screen_pack.clone()),
            listen_enabled: patch.listen_enabled.or(self.listen_enabled),
            listen_seconds: patch.listen_seconds.or(self.listen_seconds),
            voice: patch.voice.clone().or_else(|| self.voice.clone()),
            daily_cap: patch.daily_cap.or(self.daily_cap),
            tool_cap: patch.tool_cap.or(self.tool_cap),
        }
    }

    /// Encode only explicitly accepted keys for the screen-owned atomic writer.
    ///
    /// # Errors
    /// Returns a safe diagnostic if a value cannot be represented as native TOML.
    pub fn to_toml_string(&self) -> Result<String, ConfigError> {
        let mut tables: BTreeMap<&str, BTreeMap<&str, toml::Value>> = BTreeMap::new();
        if let Some(value) = self.listen_enabled {
            tables
                .entry("voice")
                .or_default()
                .insert("session_listen_enabled", value.into());
        }
        if let Some(value) = self.listen_seconds {
            tables
                .entry("voice")
                .or_default()
                .insert("session_listen_seconds", value.into());
        }
        if let Some(value) = &self.voice {
            tables.entry("codex").or_default().insert("voice", value.clone().into());
        }
        if let Some(value) = self.daily_cap {
            let value = i64::try_from(value)
                .map_err(|_| ConfigError::new("agent.daily_request_cap", "must fit a TOML integer"))?;
            tables
                .entry("agent")
                .or_default()
                .insert("daily_request_cap", value.into());
        }
        if let Some(value) = self.tool_cap {
            tables
                .entry("agent")
                .or_default()
                .insert("max_tool_calls", value.into());
        }
        let mut root: BTreeMap<&str, toml::Value> = tables
            .into_iter()
            .map(|(key, table)| {
                (
                    key,
                    toml::Value::Table(table.into_iter().map(|(k, v)| (k.into(), v)).collect()),
                )
            })
            .collect();
        if let Some(value) = self.screen_pack() {
            root.insert("screen_pack", value.into());
        }
        toml::to_string(&root).map_err(|_| ConfigError::new("settings.toml", "cannot encode overlay"))
    }

    /// Omitted means automatic template selection; empty explicitly disables it.
    /// Resolution against installed packs belongs to the screen owner.
    #[must_use]
    pub fn screen_pack(&self) -> Option<&str> {
        self.screen_pack.as_deref()
    }
}

impl BridgeConfig {
    /// Read the nominated base, then the configured home's optional settings file.
    /// Invalid settings return the validated base plus a warning for the screen.
    /// Neither this loader nor the pure parser writes files or reads credentials.
    ///
    /// # Errors
    /// An invalid or unreadable base remains a startup error.
    pub fn load_effective(path: impl AsRef<Path>) -> Result<EffectiveConfig, ConfigError> {
        let base = Self::load(path)?;
        let overlay = base.home.as_ref().map_or_else(
            || Ok(SettingsOverlay::default()),
            |home| read_overlay(&home.dir.join("settings.toml")),
        );
        match overlay.and_then(|overlay| overlay.apply_to(&base).map(|config| (overlay, config))) {
            Ok((overlay, config)) => Ok(EffectiveConfig {
                base,
                config,
                overlay,
                warning: None,
            }),
            Err(error) => Ok(EffectiveConfig {
                base: base.clone(),
                config: base,
                overlay: SettingsOverlay::default(),
                warning: Some(ConfigError::new("settings.toml", format!("ignored overlay: {error}"))),
            }),
        }
    }
}

fn read_overlay(path: &Path) -> Result<SettingsOverlay, ConfigError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(SettingsOverlay::default()),
        Err(_) => return Err(ConfigError::new("settings.toml", "cannot read overlay")),
    };
    if !metadata.is_file() {
        return Err(ConfigError::new("settings.toml", "expected regular file"));
    }
    let file = std::fs::File::open(path).map_err(|_| ConfigError::new("settings.toml", "cannot read overlay"))?;
    let mut bytes = Vec::new();
    file.take(MAX_OVERLAY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ConfigError::new("settings.toml", "cannot read overlay"))?;
    if bytes.len() as u64 > MAX_OVERLAY_BYTES {
        return Err(ConfigError::new("settings.toml", "overlay exceeds 65536 bytes"));
    }
    let text = String::from_utf8(bytes).map_err(|_| ConfigError::new("settings.toml", "expected UTF-8 overlay"))?;
    SettingsOverlay::from_toml_str(&text)
}
