//! Stage verified inputs and invoke the authored shell. No Rust lifecycle policy.
use crate::adb::Adb;
use crate::cli::{InstallPath, Options};
use crate::integrity::{self, OfflineBundle};
use crate::{Failure, Result};
use echo_contracts::secret::Secret;
use std::fmt::Write;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) mod a2;
pub mod api_key;
mod policy;

/// The pinned policy digest used in public integrity diagnostics.
pub const POLICY_CHECKSUM_FIX: &str = policy::CHECKSUM_FIX;

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
    present: impl FnMut(&crate::setup::Prompt) -> Result<()>,
    ask_wake: impl FnMut() -> Result<()>,
    instruct: impl FnMut(&str) -> Result<()>,
    cancelled: &crate::Cancellation,
) -> Result<String> {
    install_with_policy(options, present, ask_wake, instruct, cancelled, policy::fetch()).await
}

async fn install_with_policy(
    options: &Options,
    mut present: impl FnMut(&crate::setup::Prompt) -> Result<()>,
    mut ask_wake: impl FnMut() -> Result<()>,
    mut instruct: impl FnMut(&str) -> Result<()>,
    cancelled: &crate::Cancellation,
    fetch_policy: impl Future<Output = Result<Vec<u8>>>,
) -> Result<String> {
    cancelled.until(async { Ok(()) }).await?;
    let key = local_inputs(options)?;
    let scratch = Scratch::create()?;
    let policy = if options.install_path == Some(InstallPath::A2) {
        Some(cancelled.until(fetch_policy).await?)
    } else {
        None
    };
    let input = if let Some(directory) = &options.from_dir {
        Input::Offline(integrity::offline(directory, scratch.0.join("bundle"))?)
    } else {
        Input::Remote(
            cancelled
                .until(crate::release::fetch(&options.release_base, options.version()))
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
    adb.select_ready(options.serial.as_deref()).await?;
    let owner = adb.installed_owner().await?;
    if owner == InstallPath::A2 || options.install_path == Some(InstallPath::A2) {
        if options.install_path == Some(InstallPath::Legacy) {
            return Err(Failure("a2_owner_active"));
        }
        if options.command == "update" {
            return Err(Failure("command_not_implemented"));
        }
        return a2::install(
            adb,
            owner,
            (&scratch, &adapter, &input),
            options,
            (key.as_ref(), policy),
            (&mut present, &mut instruct),
            cancelled,
        )
        .await;
    }
    adb.require_legacy().await?;
    legacy(
        &adb,
        &scratch,
        (&adapter, &input),
        options,
        key.as_ref(),
        (&mut present, &mut ask_wake),
        cancelled,
    )
    .await
}

#[cfg(test)]
#[path = "../tests/a2/staging.rs"]
mod staging_tests;

async fn legacy(
    adb: &Adb,
    scratch: &Scratch,
    (adapter, input): (&std::path::Path, &Input),
    options: &Options,
    key: Option<&Secret<String>>,
    setup: (
        impl FnMut(&crate::setup::Prompt) -> Result<()>,
        impl FnMut() -> Result<()>,
    ),
    cancelled: &crate::Cancellation,
) -> Result<String> {
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
        let version = stage_install(adb, scratch, &stage, adapter, input, options).await?;
        let start = format!("sh '{stage}/bundle/scripts/echo-install.sh' --device start");
        guide_installed(adb, (&stage, key), (Some(start), &version), options, setup, cancelled).await?;
        Ok(version)
    }
    .await;
    clean_stage(adb, &stage).await.and(result)
}

async fn clean_stage(adb: &Adb, stage: &str) -> Result<()> {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        adb.shell(&format!("rm -rf '{stage}'")),
    )
    .await
    .map_err(|_| Failure("stage_cleanup_failed"))?
    .map(drop)
    .map_err(|_| Failure("stage_cleanup_failed"))
}

#[allow(clippy::disallowed_methods, reason = "Existing host time")]
async fn guide_installed(
    adb: &Adb,
    (stage, key): (&str, Option<&Secret<String>>),
    (start, version): (Option<String>, &str),
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
    let connection =
        crate::setup::InstalledConnection::selected(adb.clone(), deadline, start, Some(version.to_owned())).await?;
    if let Some(key) = key {
        if adb.owner() == InstallPath::A2 {
            connection.stop().await?;
        }
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
    let version = stage_bundle(adb, scratch, stage, adapter, input).await?;
    let bundle = format!("{stage}/bundle");
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

async fn stage_bundle(
    adb: &Adb,
    scratch: &Scratch,
    stage: &str,
    adapter: &std::path::Path,
    input: &Input,
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
            release.version.clone()
        }
        Input::Offline(input) => {
            // The destination is new under the private stage. Transfer the verified
            // snapshot recursively in one sync session, including the attribution tree.
            adb.push(&input.root, &bundle).await?;
            input.version.clone()
        }
    };
    Ok(version)
}
