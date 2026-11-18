use crate::{BridgeConfig, ConfigError, config_decode::Fields};
use std::path::PathBuf;
use toml::de::DeValue;

/// Offline settings for the app server. Runtime admission requires `home`;
/// parsing never resolves the binary, expands paths or reads credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexConfig {
    pub home: Option<PathBuf>,
    pub binary: Option<PathBuf>,
    pub voice: Option<String>,
    pub instructions: String,
    pub model: Option<String>,
    pub delegate_model: Option<String>,
    pub delegate_effort: Option<String>,
    pub time_zone: Option<String>,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            home: None,
            binary: None,
            voice: None,
            instructions: "Client audio bridge".into(),
            model: None,
            delegate_model: None,
            delegate_effort: None,
            time_zone: None,
        }
    }
}

/// The app server's `RealtimeVoice` values at the pinned rust-v0.160.0 schema.
/// `default` is ours: it omits the field so the app server picks its voice.
const REALTIME_VOICES: [&str; 19] = [
    "alloy", "arbor", "ash", "ballad", "breeze", "cedar", "coral", "cove", "echo", "ember", "juniper", "maple",
    "marin", "sage", "shimmer", "sol", "spruce", "vale", "verse",
];

impl BridgeConfig {
    /// Omit the voice when neither setting is supplied, or the chosen one is
    /// `default`, using the pinned server default.
    #[must_use]
    pub fn codex_voice(&self) -> Option<&str> {
        self.codex
            .voice
            .as_deref()
            .or(self.live.voice.as_deref())
            .filter(|voice| *voice != "default")
    }

    /// The delegated thread's model; the realtime model stays owned by the app server.
    #[must_use]
    pub fn codex_model(&self) -> Option<&str> {
        self.codex
            .delegate_model
            .as_deref()
            .or(self.codex.model.as_deref())
            .or(self.agent.model.as_deref())
    }
}

pub(super) fn decode(value: DeValue<'_>) -> Result<CodexConfig, ConfigError> {
    let mut fields = Fields::new(value, "codex")?;
    let config = CodexConfig {
        home: fields.optional("home")?,
        binary: fields.optional("binary")?,
        voice: fields.optional("voice")?,
        instructions: fields
            .optional("instructions")?
            .unwrap_or_else(|| CodexConfig::default().instructions),
        model: fields.optional("model")?,
        delegate_model: fields.optional("delegate_model")?,
        delegate_effort: fields.optional("delegate_effort")?,
        time_zone: fields.optional("time_zone")?,
    };
    fields.finish()?;
    Ok(config)
}

/// An unknown voice is refused offline: the app server rejects the whole
/// realtime start as an invalid request, after the wake.
pub(super) fn validate_voice(config: &BridgeConfig) -> Result<(), ConfigError> {
    for (field, voice) in [("codex.voice", &config.codex.voice), ("live.voice", &config.live.voice)] {
        if let Some(voice) = voice
            && !voice.trim().is_empty()
            && voice != "default"
            && !REALTIME_VOICES.contains(&voice.as_str())
        {
            return Err(ConfigError::new(field, "expected default or a pinned realtime voice"));
        }
    }
    Ok(())
}

pub(super) fn validate(config: &CodexConfig) -> Result<(), ConfigError> {
    for (field, path) in [("codex.home", &config.home), ("codex.binary", &config.binary)] {
        if path.as_ref().is_some_and(|path| !path.is_absolute()) {
            return Err(ConfigError::new(field, "expected nonempty absolute path"));
        }
    }
    for (field, value) in [
        ("codex.voice", config.voice.as_deref()),
        ("codex.model", config.model.as_deref()),
        ("codex.delegate_model", config.delegate_model.as_deref()),
        ("codex.instructions", Some(config.instructions.as_str())),
    ] {
        if value.is_some_and(|value| value.trim().is_empty()) {
            return Err(ConfigError::new(field, "must be nonblank"));
        }
    }
    if config.delegate_effort.as_ref().is_some_and(String::is_empty) {
        return Err(ConfigError::new("codex.delegate_effort", "must be nonempty"));
    }
    if let Some(zone) = &config.time_zone
        && !zone.split('/').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_+-".contains(&byte))
        })
    {
        return Err(ConfigError::new("codex.time_zone", "expected IANA-name syntax"));
    }
    Ok(())
}
