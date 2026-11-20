//! Stage verified inputs and invoke the authored shell. No Rust lifecycle policy.
use crate::adb::Adb;
use crate::cli::Options;
use crate::integrity::{self, OfflineBundle};
use crate::{Failure, Result};
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const ADAPTER: &str = crate::DEVICE_SCRIPT;
static NONCE: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn create() -> Result<Self> {
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Failure("temporary_files_failed"))?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "echo-setup-{}-{epoch}-{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder
                .mode(0o700)
                .create(&path)
                .map_err(|_| Failure("temporary_files_failed"))?;
        }
        #[cfg(not(unix))]
        builder.create(&path).map_err(|_| Failure("temporary_files_failed"))?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

enum Input {
    Remote(crate::release::Release),
    Offline(OfflineBundle),
}

pub struct Installed {
    pub version: String,
    pub screen_pack_asset: Option<String>,
}

/// Retain lifecycle mutation ownership on cancellation, then drain setup/stage resources.
/// # Errors
/// Fixed install/setup errors, with cleanup failures taking priority.
pub async fn install(
    options: &Options,
    mut present: impl FnMut(&crate::setup::Prompt) -> Result<()>,
    mut ask_wake: impl FnMut() -> Result<()>,
    cancelled: &crate::Cancellation,
) -> Result<Installed> {
    cancelled.until(async { Ok(()) }).await?;
    if options.force
        && !options
            .config
            .as_ref()
            .is_some_and(|path| path.is_file() && fs::File::open(path).is_ok())
    {
        return Err(Failure("config_unreadable"));
    }
    let scratch = Scratch::create()?;
    let input = if let Some(directory) = &options.from_dir {
        Input::Offline(integrity::offline(directory, scratch.0.join("bundle"))?)
    } else {
        Input::Remote(
            cancelled
                .until(crate::release::fetch(
                    &options.release_base,
                    &options.version,
                    options.command == "install",
                ))
                .await?,
        )
    };
    if let Some(config) = &options.config
        && !config.is_file()
    {
        return Err(Failure("config_unreadable"));
    }
    cancelled.until(async { Ok(()) }).await?;
    let adapter = scratch.0.join("adapter.sh");
    fs::write(&adapter, ADAPTER).map_err(|_| Failure("temporary_files_failed"))?;
    let (adb, stage) = open_stage(options, &scratch).await?;
    let result = async {
        let version = stage_install(&adb, &scratch, &stage, &adapter, &input, options).await?;
        guide_installed(&adb, &stage, &version, options, &mut present, &mut ask_wake, cancelled).await?;
        let screen_pack_asset = match input {
            Input::Remote(release) => release.screen_pack.map(|pack| pack.asset),
            Input::Offline(_) => None,
        };
        Ok(Installed {
            version,
            screen_pack_asset,
        })
    }
    .await;
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        adb.shell(&format!("rm -rf '{stage}'")),
    )
    .await
    .map_err(|_| Failure("stage_cleanup_failed"))?
    .map_err(|_| Failure("stage_cleanup_failed"))?;
    result
}

async fn open_stage(options: &Options, scratch: &Scratch) -> Result<(Adb, String)> {
    let mut adb = Adb::discover(options.adb.as_deref())?;
    adb.select(options.serial.as_deref()).await?;
    let (stopped, output) = adb.script("stop").await?;
    if !stopped || output != "echo-install: stopped\n" {
        return Err(Failure("stop_failed"));
    }
    let name = scratch
        .0
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or(Failure("temporary_files_failed"))?;
    let stage = format!("/data/local/tmp/{name}");
    adb.shell(&format!(
        "test ! -L /data/local/tmp && test -d /data/local/tmp && umask 077 && mkdir '{stage}'"
    ))
    .await?;
    Ok((adb, stage))
}

async fn guide_installed(
    adb: &Adb,
    stage: &str,
    version: &str,
    options: &Options,
    present: impl FnMut(&crate::setup::Prompt) -> Result<()>,
    ask_wake: impl FnMut() -> Result<()>,
    cancelled: &crate::Cancellation,
) -> Result<()> {
    cancelled.until(async { Ok(()) }).await?;
    let deadline = tokio::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(options.timeout_seconds))
        .ok_or(Failure("setup_timeout"))?;
    let start = format!("sh '{stage}/bundle/scripts/echo-install.sh' --device start");
    let connection =
        crate::setup::InstalledConnection::selected(adb.clone(), deadline, Some((start, version.to_owned()))).await?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Failure("setup_clock_invalid"))?
        .as_secs();
    connection.guide_cancellable(now, present, ask_wake, cancelled).await
}

async fn stage_install(
    adb: &Adb,
    scratch: &Scratch,
    stage: &str,
    adapter: &Path,
    input: &Input,
    options: &Options,
) -> Result<String> {
    adb.push(adapter, &format!("{stage}/adapter.sh")).await?;
    let bundle = format!("{stage}/bundle");
    let (version, pack) = match input {
        Input::Remote(release) => {
            let archive = scratch.0.join("archive.tar.gz");
            fs::write(&archive, &release.bytes).map_err(|_| Failure("temporary_files_failed"))?;
            adb.push(&archive, &format!("{stage}/archive.tar.gz")).await?;
            adb.shell(&format!("sh '{stage}/adapter.sh' --device prepare-archive --archive '{stage}/archive.tar.gz' --sha256 '{}' --stage '{stage}'", release.digest)).await
                .map_err(|_| Failure("unsafe_archive_or_missing_device_tools"))?;
            (options.version.as_str(), release.screen_pack.as_ref())
        }
        Input::Offline(input) => {
            // The destination is new under the private stage. Transfer the verified
            // snapshot recursively in one sync session, including the attribution tree.
            adb.push(&input.root, &bundle).await?;
            (input.version.as_str(), None)
        }
    };
    let identity = if let Some(pack) = pack {
        Some(prepare_pack(adb, scratch, stage, pack, version).await?)
    } else {
        None
    };
    let seed = if options.config.is_none()
        && let Some(pack) = pack
    {
        seed_config(adb, scratch, stage, version, pack).await?
    } else {
        None
    };
    install_bundle(
        adb,
        stage,
        version,
        options.config.as_deref().or(seed.as_deref()),
        options.force,
    )
    .await?;
    if let Some(identity) = identity {
        adb.shell(&format!(
            "sh '{stage}/adapter.sh' --device publish-screen-pack --identity '{identity}' --stage '{stage}'"
        ))
        .await
        .map_err(|_| Failure("install_failed"))?;
    }
    adb.shell(&format!("sh '{bundle}/scripts/echo-install.sh' --device screen-setup"))
        .await
        .map_err(|_| Failure("screen_setup_failed"))?;
    Ok(version.to_owned())
}

async fn install_bundle(adb: &Adb, stage: &str, version: &str, config: Option<&Path>, force: bool) -> Result<()> {
    let bundle = format!("{stage}/bundle");
    let mut command = format!(
        "sh '{bundle}/scripts/echo-install.sh' --device install --bundle '{bundle}' --release-version '{version}'"
    );
    if let Some(config) = config {
        adb.push(config, &format!("{stage}/config")).await?;
        let _ = write!(command, " --config '{stage}/config'");
    }
    if force {
        command.push_str(" --force");
    }
    adb.shell(&command).await.map_err(|_| Failure("install_failed"))?;
    Ok(())
}

#[derive(serde::Deserialize)]
struct PackManifest {
    format: u32,
    name: String,
    version: String,
}

async fn prepare_pack(
    adb: &Adb,
    scratch: &Scratch,
    stage: &str,
    pack: &crate::release::ScreenPack,
    version: &str,
) -> Result<String> {
    let identity = format!("{}@{version}", pack.name);
    for (name, bytes) in [
        ("screenpack.tar.gz", pack.bytes.as_slice()),
        ("screenpack.identity", identity.as_bytes()),
    ] {
        let path = scratch.0.join(name);
        fs::write(&path, bytes).map_err(|_| Failure("temporary_files_failed"))?;
        adb.push(&path, &format!("{stage}/{name}")).await?;
    }
    let invalid = Failure("unsafe_archive_or_missing_device_tools");
    let Ok(manifest) = adb
        .shell(&format!(
            "sh '{stage}/adapter.sh' --device prepare-screen-pack --sha256 '{}' --stage '{stage}'",
            pack.digest
        ))
        .await
    else {
        adb.shell(&format!("test -f '{stage}/screenpack.conflict'"))
            .await
            .map_err(|_| invalid)?;
        return Err(Failure("install_failed"));
    };
    let manifest: PackManifest = serde_json::from_str(&manifest).map_err(|_| invalid)?;
    if manifest.format != 1 || manifest.name != pack.name || manifest.version != version {
        return Err(invalid);
    }
    Ok(identity)
}

async fn seed_config(
    adb: &Adb,
    scratch: &Scratch,
    stage: &str,
    version: &str,
    pack: &crate::release::ScreenPack,
) -> Result<Option<PathBuf>> {
    let sample = adb
        .shell(&format!("head -c 1048577 '{stage}/bundle/examples/bridge.echo.toml'"))
        .await
        .map_err(|_| Failure("config_unreadable"))?;
    let Ok(mut config) = sample.parse::<toml::Value>() else {
        return Ok(None);
    };
    let Some(pins) = config
        .get_mut("screen")
        .and_then(|screen| screen.get_mut("packs"))
        .and_then(toml::Value::as_array_mut)
    else {
        return Ok(None);
    };
    let url = format!(
        "https://github.com/portpowered/portos-echo/releases/download/v{version}/{}",
        pack.asset
    );
    let before = pins.len();
    pins.retain(|pin| {
        pin.get("url").and_then(toml::Value::as_str) != Some(url.as_str())
            || pin.get("sha256").and_then(toml::Value::as_str) != Some(pack.digest.as_str())
    });
    if pins.len() == before {
        return Ok(None);
    }
    let path = scratch.0.join("config");
    let seed = toml::to_string(&config).map_err(|_| Failure("config_unreadable"))?;
    fs::write(&path, seed).map_err(|_| Failure("temporary_files_failed"))?;
    Ok(Some(path))
}
