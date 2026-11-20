//! Stage verified inputs and invoke the authored shell. No Rust lifecycle policy.
use crate::adb::Adb;
use crate::cli::Options;
use crate::integrity::{self, OfflineBundle};
use crate::{Failure, Result};
use std::fmt::Write;
use std::fs;
use std::path::PathBuf;
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

/// Integrity is established before even running `adb devices`.
/// # Errors
/// Returns fixed integrity, prerequisite or device-operation categories.
pub async fn install(options: &Options) -> Result<String> {
    let input = if let Some(directory) = &options.from_dir {
        Input::Offline(integrity::offline(directory)?)
    } else {
        Input::Remote(crate::release::fetch(&options.release_base, &options.version).await?)
    };
    if let Some(config) = &options.config
        && !config.is_file()
    {
        return Err(Failure("config_unreadable"));
    }
    let scratch = Scratch::create()?;
    let adapter = scratch.0.join("adapter.sh");
    fs::write(&adapter, ADAPTER).map_err(|_| Failure("temporary_files_failed"))?;
    let mut adb = Adb::discover(options.adb.as_deref())?;
    adb.select(options.serial.as_deref()).await?;
    if options.command == "update" && !adb.script("stop").await?.0 {
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
    let result = stage_install(&adb, &scratch, &stage, &adapter, &input, options).await;
    let cleanup = adb.shell(&format!("rm -rf '{stage}'")).await;
    let version = result?;
    cleanup.map_err(|_| Failure("stage_cleanup_failed"))?;
    Ok(version)
}

async fn stage_install(
    adb: &Adb,
    scratch: &Scratch,
    stage: &str,
    adapter: &std::path::Path,
    input: &Input,
    options: &Options,
) -> Result<String> {
    adb.push(adapter, &format!("{stage}/adapter.sh")).await?;
    let bundle = format!("{stage}/bundle");
    let version = match input {
        Input::Remote(release) => {
            let archive = scratch.0.join("archive.tar.gz");
            fs::write(&archive, &release.bytes).map_err(|_| Failure("temporary_files_failed"))?;
            adb.push(&archive, &format!("{stage}/archive.tar.gz")).await?;
            adb.shell(&format!("sh '{stage}/adapter.sh' --device prepare-archive --archive '{stage}/archive.tar.gz' --sha256 '{}' --stage '{stage}'", release.digest)).await
                .map_err(|_| Failure("unsafe_archive_or_missing_device_tools"))?;
            options.version.clone()
        }
        Input::Offline(input) => {
            adb.shell(&format!("mkdir '{bundle}'")).await?;
            for name in &input.files {
                let remote = format!("{bundle}/{name}");
                let parent = remote.rsplit_once('/').ok_or(Failure("invalid_bundle"))?.0;
                adb.shell(&format!("mkdir -p '{parent}'")).await?;
                adb.push(&input.root.join(name), &remote).await?;
            }
            input.version.clone()
        }
    };
    let mut command = format!(
        "sh '{bundle}/scripts/echo-install.sh' --device install --bundle '{bundle}' --release-version '{version}'"
    );
    if let Some(config) = &options.config {
        adb.push(config, &format!("{stage}/config")).await?;
        let _ = write!(command, " --config '{stage}/config'");
    }
    adb.shell(&command).await.map_err(|_| Failure("install_failed"))?;
    adb.shell(&format!("sh '{bundle}/scripts/echo-install.sh' --device start"))
        .await
        .map_err(|_| Failure("start_failed"))?;
    Ok(version)
}
