use super::{SettingsError, SettingsHost};
use crate::{
    bridge_muse::{Access, Http},
    bridge_settings::settings_atomic,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use echo_config::{MuseConfig, SettingsMuse, SettingsOverlay};
use echo_contracts::secret::Secret;
use echo_screen_contract::settings_v2::{
    MusePatchV2, MuseProbeReasonV2 as Reason, MuseProbeResultV2, MuseReachabilityV2 as Reachability, MuseStatusV2,
    SettingsVersionV2,
};
use std::{
    fs::{self, DirBuilder, File},
    future::Future,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(0);
const ORIGIN: SettingsError = SettingsError::Prompt("muse.url: expected HTTPS origin without inline credentials");
const CA: SettingsError = SettingsError::Prompt("muse.ca: expected a valid DER certificate");
const CREDENTIAL: SettingsError = SettingsError::Prompt("muse.credential: expected a valid service credential");

#[derive(Default)]
pub(super) struct MuseSettings {
    pub(super) access: Access,
    pub(super) local: bool,
    pub(super) observation: Option<Result<(), Reason>>,
}

impl SettingsHost {
    pub(super) fn muse_status(&self) -> MuseStatusV2 {
        let current = self.store.effective().muse.as_ref();
        let (reachability, reason) = match self.muse.observation {
            Some(Ok(())) => (Reachability::Reachable, None),
            Some(Err(reason)) if current.is_some() => (Reachability::Unreachable, Some(reason)),
            _ => (Reachability::Unchecked, None),
        };
        MuseStatusV2 {
            configured: current.is_some(),
            url: current.map(|muse| muse.url.clone()),
            reachability,
            reason,
        }
    }

    pub(in crate::bridge_settings) fn prepare_muse_probe(&self) -> impl Future<Output = Result<(), Reason>> + use<> {
        let transport = if self.store.uncertain {
            Err(Reason::Unavailable)
        } else {
            self.store
                .effective()
                .muse
                .as_ref()
                .ok_or(Reason::Unconfigured)
                .and_then(|config| Http::open(config, self.muse.local).map_err(|_| Reason::InvalidConfiguration))
        };
        async move { transport?.probe().await }
    }

    pub(in crate::bridge_settings) fn finish_muse_probe(&mut self, result: Result<(), Reason>) -> MuseProbeResultV2 {
        self.muse.observation = Some(result);
        MuseProbeResultV2 {
            version: SettingsVersionV2,
            reachable: result.is_ok(),
            reason: result.err(),
        }
    }

    pub(super) fn persist_muse(
        &mut self,
        patch: SettingsOverlay,
        input: Option<MusePatchV2>,
    ) -> Result<bool, SettingsError> {
        let Some(input) = input else {
            return self.persist(&patch);
        };
        if self.store.uncertain {
            return Err(SettingsError::Durability);
        }
        let cleared = input.clear == Some(true);
        let (muse, mut staged) = if cleared {
            (Some(SettingsMuse::Cleared), None)
        } else {
            stage(
                input,
                self.store.effective().muse.as_ref(),
                &self.store.home,
                self.muse.local,
                self.store.restart_allowed,
            )?
        };
        let changed_connection = muse.is_some();
        let saved = self.persist(&patch.with_muse(muse));
        if saved.is_ok() || matches!(saved, Err(SettingsError::Durability)) {
            if let Some(generation) = &mut staged {
                generation.referenced = true;
            }
            if changed_connection {
                self.muse.observation = None;
            }
            if cleared || matches!(saved, Err(SettingsError::Durability)) {
                self.muse.access.withdraw();
            }
        }
        saved
    }
}

fn stage(
    input: MusePatchV2,
    current: Option<&MuseConfig>,
    home: &Path,
    local: bool,
    restart_allowed: bool,
) -> Result<(Option<SettingsMuse>, Option<Generation>), SettingsError> {
    let current_url = current.and_then(|config| Http::origin(&config.url, local).ok());
    let origin = match input.url {
        Some(url) => Http::origin(&url, local).map_err(|_| ORIGIN)?,
        None => current_url.clone().ok_or(ORIGIN)?,
    };
    let same_origin = current_url.as_ref() == Some(&origin);
    if same_origin && input.ca.is_none() && input.credential.is_none() {
        return Ok((None, None));
    }
    let retained = current.filter(|_| same_origin);
    let ca = match input.ca {
        Some(value) => {
            let text = Secret::new(value.into_inner());
            Secret::new(STANDARD.decode(text.expose()).map_err(|_| CA)?)
        }
        None => Http::read_ca(&retained.ok_or(CA)?.ca_file).map_err(|_| CA)?,
    };
    let credential = match input.credential {
        Some(value) => Secret::new(value.into_inner().into_bytes()),
        None => Http::read_credential(&retained.ok_or(CREDENTIAL)?.credential_file).map_err(|_| CREDENTIAL)?,
    };
    Http::validate_credential(&credential).map_err(|_| CREDENTIAL)?;
    Http::from_inputs(origin.clone(), &ca, &credential).map_err(|_| CA)?;
    if !restart_allowed {
        return Err(SettingsError::RestartUnavailable);
    }
    let (directory, generation) = Generation::create(home).map_err(|_| SettingsError::Persistence)?;
    for (name, bytes) in [("ca.der", &ca), ("credential", &credential)] {
        settings_atomic::replace(&generation.directory, name, bytes.expose(), |_| Ok(()))
            .map_err(|_| SettingsError::Persistence)?;
    }
    let url = origin.origin().ascii_serialization();
    let config = MuseConfig {
        url,
        ca_file: directory.join("ca.der"),
        credential_file: directory.join("credential"),
    };
    Ok((Some(SettingsMuse::Configured(config)), Some(generation)))
}

struct Generation {
    directory: PathBuf,
    referenced: bool,
}

impl Generation {
    fn create(home: &Path) -> std::io::Result<(PathBuf, Self)> {
        let parent = home.join("muse");
        if !fs::symlink_metadata(home)?.is_dir() {
            return Err(std::io::Error::other("unsafe private home"));
        }
        crate::bridge_private::directory(&parent)?;
        if !private_directory(&fs::symlink_metadata(&parent)?)? {
            return Err(std::io::Error::other("unsafe Muse directory"));
        }
        File::open(home)?.sync_all()?;
        let mut builder = DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        loop {
            let sequence = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
            let relative = Path::new("muse").join(format!("connection-{}-{sequence}", std::process::id()));
            let directory = home.join(&relative);
            match builder.create(&directory) {
                Ok(()) => {
                    let generation = Self {
                        directory,
                        referenced: false,
                    };
                    File::open(&parent)?.sync_all()?;
                    return Ok((relative, generation));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
    }
}

fn private_directory(metadata: &fs::Metadata) -> std::io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let owner = fs::metadata("/proc/self")?.uid();
        Ok(metadata.is_dir() && metadata.uid() == owner && metadata.mode().trailing_zeros() >= 6)
    }
    #[cfg(not(unix))]
    Ok(metadata.is_dir())
}

impl Drop for Generation {
    fn drop(&mut self) {
        if !self.referenced {
            let _ = fs::remove_file(self.directory.join("ca.der"));
            let _ = fs::remove_file(self.directory.join("credential"));
            let _ = fs::remove_dir(&self.directory);
        }
    }
}
