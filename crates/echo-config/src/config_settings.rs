//! The screen's sparse, non-secret overlay. Never rewrites the operator's base.
use crate::{AgentTools, BridgeConfig, ConfigError, McpAuth, WebSearch, config_decode::Fields};
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
    options: SettingsOptions,
}

/// Additional sparse leaves admitted by Settings V2.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SettingsOptions {
    pub web_search: Option<WebSearch>,
    pub delegate_model: Option<String>,
    pub delegate_effort: Option<String>,
    pub agent_tools: Option<Vec<String>>,
    pub scopes: Option<Vec<String>>,
    pub server: Option<SettingsServerTools>,
}

/// Only the active Port OS server's tool allowlist is editable.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsServerTools {
    pub name: String,
    pub allow_tools: Vec<String>,
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
    pub deprecations: Vec<ConfigError>,
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
            options: SettingsOptions::default(),
        }
    }

    #[must_use]
    pub fn with_options(mut self, options: SettingsOptions) -> Self {
        self.options = options;
        self
    }

    /// Whether the overlay manages Port OS permissions in this generation.
    #[must_use]
    pub fn manages_portos(&self) -> bool {
        self.options.scopes.is_some() || self.options.server.is_some()
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
            result.options.web_search = codex.optional("web_search")?;
            result.options.delegate_model = codex.optional("delegate_model")?;
            result.options.delegate_effort = codex.optional("delegate_effort")?;
            codex.finish()?;
        }
        if let Some(mut agent) = fields.section("agent")? {
            result.daily_cap = agent.optional("daily_request_cap")?;
            result.tool_cap = agent.optional("max_tool_calls")?;
            agent.discard("task_deadline_seconds");
            agent.finish()?;
        }
        if let Some(mut home) = fields.section("home")? {
            result.options.agent_tools = home.optional("agent_tools")?;
            home.finish()?;
        }
        if let Some(mut mcp) = fields.section("mcp")? {
            result.options.scopes = mcp.optional("scopes")?;
            let servers: Option<Vec<SettingsServerTools>> = mcp.optional("server")?;
            if let Some(mut servers) = servers {
                if servers.len() != 1 {
                    return Err(ConfigError::new("mcp.server", "expected one Port OS server override"));
                }
                result.options.server = servers.pop();
            }
            mcp.finish()?;
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
        if self.tool_cap.is_some_and(|value| !(1..=32).contains(&value)) {
            return Err(ConfigError::new("agent.max_tool_calls", "must be within 1..=32"));
        }
        self.options.apply_to(&mut config)?;
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
            options: self.options.merge(&patch.options),
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
        self.options.encode(&mut tables)?;
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
        let text = toml::to_string(&root).map_err(|_| ConfigError::new("settings.toml", "cannot encode overlay"))?;
        if text.len() as u64 > MAX_OVERLAY_BYTES {
            return Err(ConfigError::new("settings.toml", "overlay exceeds 65536 bytes"));
        }
        Ok(text)
    }

    /// Stored tool cap for Settings V1 compatibility.
    #[must_use]
    pub fn legacy_tool_cap(&self) -> Option<u32> {
        self.tool_cap
    }

    /// Omitted means automatic template selection; empty explicitly disables it.
    /// Resolution against installed packs belongs to the screen owner.
    #[must_use]
    pub fn screen_pack(&self) -> Option<&str> {
        self.screen_pack.as_deref()
    }
}

impl SettingsOptions {
    fn apply_to(&self, config: &mut BridgeConfig) -> Result<(), ConfigError> {
        if let Some(mode) = self.web_search {
            config.codex.web_search = mode;
        }
        if let Some(model) = &self.delegate_model {
            config.codex.delegate_model = nonempty_override(model);
            if model.is_empty() {
                // An explicit screen default outranks both legacy aliases.
                config.codex.model = None;
                config.agent.model = None;
            }
        }
        if let Some(effort) = &self.delegate_effort {
            config.codex.delegate_effort = nonempty_override(effort);
        }
        if let Some(groups) = &self.agent_tools {
            let home = config
                .home
                .as_mut()
                .ok_or(ConfigError::new("home.agent_tools", "requires configured home"))?;
            home.agent_tools = home_tools(groups)?;
        }
        self.apply_portos(config)
    }

    fn apply_portos(&self, config: &mut BridgeConfig) -> Result<(), ConfigError> {
        if self.scopes.is_none() && self.server.is_none() {
            return Ok(());
        }
        let server = config
            .mcp
            .server
            .first_mut()
            .filter(|server| server.auth == McpAuth::PortosDeviceCode)
            .ok_or(ConfigError::new("mcp.server", "expected active Port OS server"))?;
        if let Some(tools) = &self.server {
            if tools.name != server.name {
                return Err(ConfigError::new("mcp.server", "Port OS server name does not match"));
            }
            server.allow_tools.clone_from(&tools.allow_tools);
        }
        if let Some(scopes) = &self.scopes {
            config.mcp.scopes.clone_from(scopes);
        }
        Ok(())
    }

    fn merge(&self, patch: &Self) -> Self {
        Self {
            web_search: patch.web_search.or(self.web_search),
            delegate_model: patch.delegate_model.clone().or_else(|| self.delegate_model.clone()),
            delegate_effort: patch.delegate_effort.clone().or_else(|| self.delegate_effort.clone()),
            agent_tools: patch.agent_tools.clone().or_else(|| self.agent_tools.clone()),
            scopes: patch.scopes.clone().or_else(|| self.scopes.clone()),
            server: patch.server.clone().or_else(|| self.server.clone()),
        }
    }

    fn encode(&self, tables: &mut BTreeMap<&str, BTreeMap<&str, toml::Value>>) -> Result<(), ConfigError> {
        let mut leaf = |table, key, value| {
            if let Some(value) = value {
                tables.entry(table).or_default().insert(key, value);
            }
        };
        leaf("codex", "web_search", self.web_search.map(|mode| mode.as_str().into()));
        leaf("codex", "delegate_model", self.delegate_model.clone().map(Into::into));
        leaf("codex", "delegate_effort", self.delegate_effort.clone().map(Into::into));
        leaf("home", "agent_tools", self.agent_tools.clone().map(toml_array));
        leaf("mcp", "scopes", self.scopes.clone().map(toml_array));
        if let Some(server) = &self.server {
            let value =
                toml::Value::try_from(server).map_err(|_| ConfigError::new("mcp.server", "cannot encode overlay"))?;
            leaf("mcp", "server", Some(toml::Value::Array(vec![value])));
        }
        Ok(())
    }
}

fn nonempty_override(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn home_tools(groups: &[String]) -> Result<AgentTools, ConfigError> {
    let mut tools = AgentTools {
        notes: false,
        prompts: false,
    };
    for group in groups {
        let enabled = match group.as_str() {
            "notes" => &mut tools.notes,
            "prompts" => &mut tools.prompts,
            _ => return Err(ConfigError::new("home.agent_tools", "unsupported group")),
        };
        if std::mem::replace(enabled, true) {
            return Err(ConfigError::new("home.agent_tools", "duplicate group"));
        }
    }
    Ok(tools)
}

fn toml_array(values: Vec<String>) -> toml::Value {
    toml::Value::Array(values.into_iter().map(Into::into).collect())
}

impl BridgeConfig {
    /// Read the nominated base, then the configured home's optional settings file.
    /// Invalid settings return the validated base plus a warning for the screen.
    /// Neither this loader nor the pure parser writes files or reads credentials.
    ///
    /// # Errors
    /// An invalid or unreadable base remains a startup error.
    pub fn load_effective(path: impl AsRef<Path>) -> Result<EffectiveConfig, ConfigError> {
        let (base, deprecations) = Self::load_with_warnings(path)?;
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
                deprecations,
            }),
            Err(error) => Ok(EffectiveConfig {
                base: base.clone(),
                config: base,
                overlay: SettingsOverlay::default(),
                warning: Some(ConfigError::new("settings.toml", format!("ignored overlay: {error}"))),
                deprecations,
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
