use crate::{ConfigError, MuseConfig, config_decode::Fields, config_muse::decode_fields};
use std::{
    collections::BTreeMap,
    fs::symlink_metadata,
    path::{Path, PathBuf},
};

/// References to one private, bridge-owned generation or an explicit withdrawal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsMuse {
    Configured(MuseConfig),
    Cleared,
}

impl SettingsMuse {
    pub(super) fn decode(mut fields: Fields<'_>) -> Result<Self, ConfigError> {
        if let Some(enabled) = fields.optional::<bool>("enabled")? {
            if enabled {
                return Err(ConfigError::new("muse.enabled", "expected false"));
            }
            fields.finish()?;
            return Ok(Self::Cleared);
        }
        let muse = decode_fields(fields)?;
        generation(&muse)?;
        Ok(Self::Configured(muse))
    }

    pub(super) fn resolve(&self, home: Option<&Path>) -> Result<Option<MuseConfig>, ConfigError> {
        let Self::Configured(muse) = self else { return Ok(None) };
        let home = home.ok_or_else(path_error)?;
        let directory = home.join(generation(muse)?);
        for path in [home, &home.join("muse"), &directory] {
            if !symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
                return Err(path_error());
            }
        }
        let resolve = |path: &Path| {
            let path = home.join(path);
            symlink_metadata(&path)
                .is_ok_and(|metadata| metadata.is_file())
                .then_some(path)
                .ok_or_else(path_error)
        };
        Ok(Some(MuseConfig {
            url: muse.url.clone(),
            ca_file: resolve(&muse.ca_file)?,
            credential_file: resolve(&muse.credential_file)?,
        }))
    }

    pub(super) fn encode(&self) -> BTreeMap<&'static str, toml::Value> {
        match self {
            Self::Cleared => BTreeMap::from([("enabled", false.into())]),
            Self::Configured(muse) => BTreeMap::from([
                ("url", muse.url.clone().into()),
                ("ca_file", muse.ca_file.to_string_lossy().into_owned().into()),
                (
                    "credential_file",
                    muse.credential_file.to_string_lossy().into_owned().into(),
                ),
            ]),
        }
    }
}

fn generation(muse: &MuseConfig) -> Result<PathBuf, ConfigError> {
    // Check the raw components as well as the eventual file type. Path's
    // normalization would otherwise accept repeated separators and dot segments.
    let ca = muse.ca_file.to_str().ok_or_else(path_error)?;
    let (muse_dir, name, file) = ca
        .split_once('/')
        .and_then(|(dir, tail)| tail.split_once('/').map(|(name, file)| (dir, name, file)))
        .ok_or_else(path_error)?;
    let valid = name.strip_prefix("connection-").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    });
    if !valid
        || muse_dir != "muse"
        || file != "ca.der"
        || muse.credential_file.to_str() != Some(format!("muse/{name}/credential").as_str())
    {
        return Err(path_error());
    }
    Ok(Path::new("muse").join(name))
}

fn path_error() -> ConfigError {
    ConfigError::new("muse", "expected private connection files under home")
}
