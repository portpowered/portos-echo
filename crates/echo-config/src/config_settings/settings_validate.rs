//! Checks a decoded overlay must pass after every field is read.
use super::SettingsOverlay;
use crate::ConfigError;

impl SettingsOverlay {
    pub(super) fn validate(&self) -> Result<(), ConfigError> {
        if let Some(name) = &self.endpoint.device_name {
            crate::config_endpoint::validate_name(name)?;
        }
        if let Some(value) = self.daily_cap {
            i64::try_from(value).map_err(|_| ConfigError::new("agent.daily_request_cap", "must fit a TOML integer"))?;
        }
        Ok(())
    }
}
