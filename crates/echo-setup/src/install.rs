//! Stage verified inputs and invoke the authored shell. No Rust lifecycle policy.
use crate::adb::Adb;
use crate::cli::Options;
use crate::integrity::{self, OfflineBundle};
use crate::{Failure, Result};
use echo_contracts::secret::Secret;
use std::fmt::Write;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

pub mod api_key;

const ADAPTER: &str = crate::DEVICE_SCRIPT;
static NONCE: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    #[allow(clippy::disallowed_methods, reason = "Existing host time")]
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

/// Retain lifecycle mutation ownership on cancellation, then drain setup/stage resources.
/// # Errors
/// Fixed install/setup errors, with cleanup failures taking priority.
pub async fn install(
    options: &Options,
    mut present: impl FnMut(&crate::setup::Prompt) -> Result<()>,
    mut ask_wake: impl FnMut() -> Result<()>,
    cancelled: &crate::Cancellation,
) -> Result<String> {
    cancelled.until(async { Ok(()) }).await?;
    let key = local_inputs(options)?;
    let scratch = Scratch::create()?;
    let input = if let Some(directory) = &options.from_dir {
        Input::Offline(integrity::offline(directory, scratch.0.join("bundle"))?)
    } else {
        Input::Remote(
            cancelled
                .until(crate::release::fetch(&options.release_base, &options.version))
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
    let result = async {
        let version = stage_install(&adb, &scratch, &stage, &adapter, &input, options).await?;
        let setup = (&mut present, &mut ask_wake);
        guide_installed(&adb, (&stage, key.as_ref()), &version, options, setup, cancelled).await?;
        Ok(version)
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

#[allow(clippy::disallowed_methods, reason = "Existing host time")]
async fn guide_installed(
    adb: &Adb,
    (stage, key): (&str, Option<&Secret<String>>),
    version: &str,
    options: &Options,
    (present, ask_wake): (
        impl FnMut(&crate::setup::Prompt) -> Result<()>,
        impl FnMut() -> Result<()>,
    ),
    cancelled: &crate::Cancellation,
) -> Result<()> {
    cancelled.until(async { Ok(()) }).await?;
    let deadline = tokio::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(options.timeout_seconds))
        .ok_or(Failure("setup_timeout"))?;
    let start = format!("sh '{stage}/bundle/scripts/echo-install.sh' --device start");
    let connection =
        crate::setup::InstalledConnection::selected(adb.clone(), deadline, Some((start, version.to_owned()))).await?;
    if let Some(key) = key {
        // The installed config decides the mode and path; the bridge is stopped.
        connection.install_api_key(stage, key).await?;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Failure("setup_clock_invalid"))?
        .as_secs();
    connection.guide_cancellable(now, present, ask_wake, cancelled).await
}

/// Check local inputs before any download or device effect: a forced config
/// must be readable, and a key needs a config that does not keep `ChatGPT` sign-in.
fn local_inputs(options: &Options) -> Result<Option<Secret<String>>> {
    if options.force
        && !options
            .config
            .as_ref()
            .is_some_and(|path| path.is_file() && fs::File::open(path).is_ok())
    {
        return Err(Failure("config_unreadable"));
    }
    let Some(path) = &options.openai_api_key_file else {
        return Ok(None);
    };
    let key = api_key::read(path)?;
    if let Some(config) = &options.config {
        let raw = fs::read_to_string(config).map_err(|_| Failure("config_unreadable"))?;
        if api_key::project(&raw)?.0 != api_key::AuthMode::ApiKey {
            return Err(Failure("api_key_config_mode_chatgpt"));
        }
    }
    Ok(Some(key))
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
            // The destination is new under the private stage. Transfer the verified
            // snapshot recursively in one sync session, including the attribution tree.
            adb.push(&input.root, &bundle).await?;
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
    if options.force {
        command.push_str(" --force");
    }
    adb.shell(&command).await.map_err(|_| Failure("install_failed"))?;
    adb.shell(&format!("sh '{bundle}/scripts/echo-install.sh' --device screen-setup"))
        .await
        .map_err(|_| Failure("screen_setup_failed"))?;
    Ok(version)
}
