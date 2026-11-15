//! Synchronous bridge configuration. Loaders read only the nominated TOML file;
//! credential paths remain literal and are never opened or expanded.

mod config_decode;
mod config_types;
mod config_validate;

#[cfg(test)]
mod config_rule_tests;

pub use config_types::*;

use std::path::Path;

/// A diagnostic containing only a field path and a fixed category, never input values.
#[derive(Debug, thiserror::Error)]
#[error("{field}: {message}")]
pub struct ConfigError {
    field: String,
    message: &'static str,
}

impl ConfigError {
    fn new(field: impl Into<String>, message: &'static str) -> Self {
        Self {
            field: field.into(),
            message,
        }
    }
}

impl BridgeConfig {
    /// Parse and validate TOML without reading credentials or contacting services.
    ///
    /// # Errors
    /// Returns a safe field-specific diagnostic for invalid configuration.
    pub fn from_toml_str(input: &str) -> Result<Self, ConfigError> {
        let table = toml::de::DeTable::parse(input).map_err(|_| ConfigError::new("config", "invalid TOML syntax"))?;
        let value = toml::de::DeValue::Table(table.into_inner());
        config_validate::reject_secrets(&value, "")?;
        let config = config_decode::decode(value)?;
        config_validate::validate(&config)?;
        Ok(config)
    }

    /// Read only `path`, then use the same validation as [`Self::from_toml_str`].
    /// Relative credential paths are preserved, including on repeated loads.
    ///
    /// # Errors
    /// Returns a safe config diagnostic for I/O/encoding errors, or a field error.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let input = std::fs::read_to_string(path)
            .map_err(|_| ConfigError::new("config", "cannot read UTF-8 configuration file"))?;
        Self::from_toml_str(&input)
    }
}
