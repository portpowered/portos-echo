//! Offline screen-pack configuration and settings identifier validation.
use crate::{BridgeConfig, ConfigError};

/// One pinned archive. URLs and digests are data, never diagnostic text.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenPackDownload {
    pub url: String,
    pub sha256: String,
}

pub(super) fn validate(config: &BridgeConfig) -> Result<(), ConfigError> {
    if !config.screen.packs.is_empty() && config.home.is_none() {
        return Err(ConfigError::new("screen.packs", "requires home.dir"));
    }
    for (index, pack) in config.screen.packs.iter().enumerate() {
        let field = format!("screen.packs[{index}]");
        if !pack
            .url
            .get(..8)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
            || pack.url.contains('#')
            || !crate::config_validate::http_url(&pack.url)
        {
            return Err(ConfigError::new(
                format!("{field}.url"),
                "expected HTTPS URL without credentials or fragment",
            ));
        }
        if pack.sha256.len() != 64 || !pack.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ConfigError::new(
                format!("{field}.sha256"),
                "expected 64 hexadecimal digits",
            ));
        }
    }
    Ok(())
}

pub(super) fn selection(value: &str) -> Result<(), ConfigError> {
    if value.is_empty() {
        return Ok(());
    }
    let (name, version) = value
        .split_once('@')
        .map_or((value, None), |(name, version)| (name, Some(version)));
    let name_valid = name.len() <= 48
        && name.bytes().next().is_some_and(|byte| byte.is_ascii_lowercase())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if !name_valid || version.is_some_and(|version| !valid_version(version)) {
        return Err(ConfigError::new(
            "screen_pack",
            "expected pack name or name@version, or empty to disable",
        ));
    }
    Ok(())
}

fn valid_version(version: &str) -> bool {
    let (release, prerelease) = version
        .split_once('-')
        .map_or((version, None), |(release, pre)| (release, Some(pre)));
    let parts: Vec<_> = release.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && prerelease.is_none_or(|pre| {
            pre.split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        })
}
