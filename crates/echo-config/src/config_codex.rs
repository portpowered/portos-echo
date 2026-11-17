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
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            home: None,
            binary: None,
            voice: None,
            instructions: "Client audio bridge".into(),
            model: None,
        }
    }
}

impl BridgeConfig {
    /// Omit the voice when neither setting is supplied, using the pinned server default.
    #[must_use]
    pub fn codex_voice(&self) -> Option<&str> {
        self.codex.voice.as_deref().or(self.live.voice.as_deref())
    }

    /// The delegated thread's model; the realtime model stays owned by the app server.
    #[must_use]
    pub fn codex_model(&self) -> Option<&str> {
        self.codex.model.as_deref().or(self.agent.model.as_deref())
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
    };
    fields.finish()?;
    Ok(config)
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
        ("codex.instructions", Some(config.instructions.as_str())),
    ] {
        if value.is_some_and(|value| value.trim().is_empty()) {
            return Err(ConfigError::new(field, "must be nonblank"));
        }
    }
    Ok(())
}
