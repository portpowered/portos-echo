use crate::{ConfigError, config_decode::Fields};

/// Base configuration for the on-device snapshot provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraConfig {
    pub enabled: bool,
    pub id: String,
}

impl Default for CameraConfig {
    fn default() -> Self {
        Self {
            enabled: cfg!(target_os = "android"),
            id: "0".into(),
        }
    }
}

pub(super) fn decode(value: Option<toml::de::DeValue<'_>>) -> Result<CameraConfig, ConfigError> {
    let Some(value) = value else {
        return Ok(CameraConfig::default());
    };
    let mut fields = Fields::new(value, "camera")?;
    let defaults = CameraConfig::default();
    let enabled = fields.optional("enabled")?.unwrap_or(defaults.enabled);
    let id = fields.optional("id")?.unwrap_or(defaults.id);
    fields.finish()?;
    if id.trim().is_empty() || id.chars().any(char::is_control) {
        return Err(ConfigError::new(
            "camera.id",
            "expected a nonblank camera ID without controls",
        ));
    }
    Ok(CameraConfig { enabled, id })
}
