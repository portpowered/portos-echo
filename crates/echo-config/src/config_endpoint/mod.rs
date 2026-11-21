use crate::{ConfigError, config_decode::Fields, config_validate::http_url};
use std::collections::BTreeMap;

const SECTION: &str = "portos_endpoint";
const NAME: &str = "portos_endpoint.device_name";
const GATEWAY_SUFFIX: &str = "/devices/{authLinkId}";
/// The Port OS API base whose device gateway is known without configuration.
pub const PORTOS_API_BASE: &str = "https://api.portoperatingsystem.com";
/// The device gateway of [`PORTOS_API_BASE`].
pub const PORTOS_GATEWAY_URL: &str = "wss://devices.portoperatingsystem.com/devices/{authLinkId}";

/// Operator configuration for the opt-in Port OS endpoint transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortosEndpoint {
    pub enabled: bool,
    pub remote_mic: bool,
    pub device_name: String,
    pub gateway_url: Option<String>,
    pub api_base: Option<String>,
}

impl Default for PortosEndpoint {
    fn default() -> Self {
        Self {
            enabled: false,
            remote_mic: false,
            device_name: "Echo Show".into(),
            gateway_url: None,
            api_base: None,
        }
    }
}

impl PortosEndpoint {
    /// The gateway template for `api_base`: the configured one, else the Port OS
    /// gateway when `api_base` is the Port OS API. Any other API needs it configured.
    #[must_use]
    pub fn gateway_for(&self, api_base: &str) -> Option<String> {
        self.gateway_url
            .clone()
            .or_else(|| (api_base.trim_end_matches('/') == PORTOS_API_BASE).then(|| PORTOS_GATEWAY_URL.into()))
    }

    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        validate_name(&self.device_name)?;
        if self.gateway_url.as_deref().is_some_and(|url| !gateway_template(url)) {
            return Err(ConfigError::new(
                "portos_endpoint.gateway_url",
                "expected a WSS URL ending in /devices/{authLinkId}",
            ));
        }
        if self.api_base.as_deref().is_some_and(|url| !https_base(url)) {
            return Err(ConfigError::new(
                "portos_endpoint.api_base",
                "expected an HTTPS URL without credentials, query, fragment, or substitutions",
            ));
        }
        Ok(())
    }
}

fn gateway_template(url: &str) -> bool {
    url.strip_prefix("wss://")
        .and_then(|tail| tail.strip_suffix(GATEWAY_SUFFIX))
        .is_some_and(|base| https_base(&format!("https://{base}/devices/")))
}

fn https_base(url: &str) -> bool {
    url.starts_with("https://") && !url.contains(['?', '#', '{', '}']) && http_url(url)
}

pub(crate) fn validate_name(name: &str) -> Result<(), ConfigError> {
    if name.trim().is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(ConfigError::new(
            NAME,
            "expected 1 to 64 characters, nonblank and without controls",
        ));
    }
    Ok(())
}

pub(super) fn decode(value: toml::de::DeValue<'_>) -> Result<PortosEndpoint, ConfigError> {
    let mut fields = Fields::new(value, "portos_endpoint")?;
    let values = PortosEndpoint {
        enabled: fields.optional("enabled")?.unwrap_or(false),
        remote_mic: fields.optional("remote_mic")?.unwrap_or(false),
        device_name: fields
            .optional("device_name")?
            .unwrap_or_else(|| PortosEndpoint::default().device_name),
        gateway_url: fields.optional("gateway_url")?,
        api_base: fields.optional("api_base")?,
    };
    fields.finish()?;
    Ok(values)
}

/// The only endpoint values editable through the paired Settings overlay.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SettingsEndpoint {
    pub enabled: Option<bool>,
    pub device_name: Option<String>,
}

impl SettingsEndpoint {
    pub(crate) fn apply_to(&self, endpoint: &mut PortosEndpoint) {
        if let Some(enabled) = self.enabled {
            endpoint.enabled = enabled;
        }
        if let Some(name) = &self.device_name {
            endpoint.device_name.clone_from(name);
        }
    }

    pub(crate) fn merge(&self, patch: &Self) -> Self {
        Self {
            enabled: patch.enabled.or(self.enabled),
            device_name: patch.device_name.clone().or_else(|| self.device_name.clone()),
        }
    }

    pub(crate) fn encode(&self, tables: &mut BTreeMap<&str, BTreeMap<&str, toml::Value>>) {
        if let Some(enabled) = self.enabled {
            tables.entry(SECTION).or_default().insert("enabled", enabled.into());
        }
        if let Some(name) = &self.device_name {
            tables
                .entry(SECTION)
                .or_default()
                .insert("device_name", name.clone().into());
        }
    }
}
