//! Offline network-time policy. Server names are literal and never resolved here.
use crate::{ConfigError, config_decode::Fields};

/// Public SNTP servers the bridge measures its clock against.
pub const DEFAULT_TIME_SERVERS: [&str; 4] = ["0.pool.ntp.org", "1.pool.ntp.org", "2.pool.ntp.org", "3.pool.ntp.org"];

/// `servers` absent selects [`DEFAULT_TIME_SERVERS`]; an empty list turns measurement off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockConfig {
    pub servers: Option<Vec<String>>,
    pub interval_seconds: u32,
}

impl Default for ClockConfig {
    fn default() -> Self {
        Self {
            servers: None,
            interval_seconds: 3600,
        }
    }
}

impl ClockConfig {
    /// The configured servers, or the defaults when none were named.
    #[must_use]
    pub fn effective_servers(&self) -> Vec<String> {
        self.servers
            .clone()
            .unwrap_or_else(|| DEFAULT_TIME_SERVERS.map(String::from).to_vec())
    }
}

/// An absent `[clock]` table is the defaults.
pub(super) fn decode(value: Option<toml::de::DeValue<'_>>) -> Result<ClockConfig, ConfigError> {
    let Some(value) = value else {
        return Ok(ClockConfig::default());
    };
    let mut fields = Fields::new(value, "clock")?;
    let result = ClockConfig {
        servers: fields.optional("servers")?,
        interval_seconds: fields.optional("interval_seconds")?.unwrap_or(3600),
    };
    fields.finish()?;
    Ok(result)
}

pub(super) fn validate(clock: &ClockConfig) -> Result<(), ConfigError> {
    if !(60..=86_400).contains(&clock.interval_seconds) {
        return Err(ConfigError::new("clock.interval_seconds", "must be within 60..=86400"));
    }
    let servers = clock.servers.as_deref().unwrap_or_default();
    if servers.len() > 8 || !servers.iter().all(|server| server_name(server)) {
        return Err(ConfigError::new(
            "clock.servers",
            "expected at most 8 host or host:port names",
        ));
    }
    Ok(())
}

// A DNS name or IPv4 literal with an optional nonzero port; no scheme, path or userinfo.
fn server_name(text: &str) -> bool {
    let (host, port) = text
        .rsplit_once(':')
        .map_or((text, None), |(host, port)| (host, Some(port)));
    port.is_none_or(|port| port.parse::<u16>().is_ok_and(|port| port != 0))
        && !host.is_empty()
        && host.len() <= 253
        && host
            .split('.')
            .all(|label| !label.is_empty() && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'))
}
