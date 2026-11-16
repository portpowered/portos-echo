//! Offline Muse profile selection. Paths are literal and never opened here.
use crate::{BridgeConfig, ConfigError, config_decode::Fields, config_validate::http_url};
use std::path::PathBuf;

/// Exactly one delegation executor is selected for an install.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentBackend {
    #[default]
    Client,
    Muse,
}

/// The authenticated Muse service origin and separately provisioned local files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuseConfig {
    pub url: String,
    pub ca_file: PathBuf,
    pub credential_file: PathBuf,
}

pub(super) fn decode(value: toml::de::DeValue<'_>) -> Result<MuseConfig, ConfigError> {
    let mut fields = Fields::new(value, "muse")?;
    let result = MuseConfig {
        url: fields.required("url")?,
        ca_file: fields.required("ca_file")?,
        credential_file: fields.required("credential_file")?,
    };
    fields.finish()?;
    Ok(result)
}

pub(super) fn validate(config: &BridgeConfig) -> Result<(), ConfigError> {
    let Some(muse) = &config.muse else {
        return if config.agent_backend == AgentBackend::Muse {
            Err(ConfigError::new("muse", "required for muse backend"))
        } else {
            Ok(())
        };
    };
    if !https_origin(&muse.url) {
        return Err(ConfigError::new(
            "muse.url",
            "expected HTTPS origin without inline credentials",
        ));
    }
    for (field, path) in [("ca_file", &muse.ca_file), ("credential_file", &muse.credential_file)] {
        if path.as_os_str().to_str().is_none_or(|value| value.trim().is_empty()) {
            return Err(ConfigError::new(format!("muse.{field}"), "expected nonempty path"));
        }
    }
    Ok(())
}

fn https_origin(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    if !http_url(url) || rest.contains(['?', '#']) {
        return false;
    }
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.contains('/') {
        return false;
    }
    let port = if authority.starts_with('[') {
        authority.split_once(']').map_or("", |(_, suffix)| suffix)
    } else {
        authority.find(':').map_or("", |offset| &authority[offset..])
    };
    port.is_empty()
        || port
            .strip_prefix(':')
            .is_some_and(|p| p.parse::<u16>().is_ok_and(|p| p != 0))
}
