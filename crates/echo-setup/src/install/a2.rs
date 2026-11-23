//! Host-side orchestration for the Android install transaction.
use super::{Input, Scratch, clean_stage, guide_installed, stage_bundle};
use crate::{
    Cancellation, Failure, Result,
    adb::Adb,
    cli::{InstallPath, Options},
    setup::Prompt,
};
use echo_contracts::secret::Secret;
use std::{fs, path::Path, time::Duration};
use tokio::time::Instant;

const ROOT: &str = "/data/local/echo";
const LOCAL: &str = "/data/local";
const PROTECTED_LOCAL: &str =
    "test ! -L /data/local; case \"$(stat -c %u:%g:%a /data/local)\" in 0:0:751|0:0:755) ;; *) exit 1;; esac";
const ROOTED: &str = "id -ru && id -u";
const BOOT_ID: &str = "cat /proc/sys/kernel/random/boot_id";
const WAIT: Duration = Duration::from_millis(250);

pub(super) async fn install(
    mut adb: Adb,
    owner: InstallPath,
    (scratch, adapter, input): (&Scratch, &Path, &Input),
    options: &Options,
    (key, policy): (Option<&Secret<String>>, Option<Vec<u8>>),
    (present, mut instruct): (impl FnMut(&Prompt) -> Result<()>, impl FnMut(&str) -> Result<()>),
    cancelled: &Cancellation,
) -> Result<String> {
    instruct("1. Verify the release and select one rooted Echo.")?;
    if adb.shell(ROOTED).await?.trim() != "0\n0" {
        return Err(Failure("a2_root_required"));
    }
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(options.timeout_seconds))
        .ok_or(Failure("setup_timeout"))?;
    instruct("2. Stage and install A2, then wait for reboot.")?;
    let configure = options.force || key.is_some();
    let version = if owner == InstallPath::A2 {
        resume(&adb, input, options.requested_version.as_deref()).await?
    } else {
        let boot = boot_id(&adb).await?;
        let installed = activate(&adb, scratch, (adapter, input), options.config.as_deref(), policy).await?;
        adb.set_a2();
        if configure && !adb.script("request-park").await?.0 {
            return Err(Failure("stop_failed"));
        }
        // Reboot can close adb before it returns. A different boot ID and a
        // completed boot on the retained device are the authoritative receipt.
        let _ = tokio::time::timeout(Duration::from_secs(10), adb.shell("reboot")).await;
        rebooted(&adb, &boot, deadline).await?;
        installed
    };
    if configure {
        configure_private(&adb, options, key, deadline).await?;
    } else {
        resume_supervisor(&adb).await?;
    }
    started(&adb, &version, deadline).await?;
    instruct("3. Complete ChatGPT sign-in, then Port OS linking when required.")?;
    let wake = || instruct("4. Say Computer. Confirm a fresh wake and run doctor.");
    guide_installed(&adb, ("", None), (None, &version), options, (present, wake), cancelled).await?;
    let doctor = cancelled.until(crate::doctor::selected(&adb)).await?;
    if !doctor.0.iter().all(|check| *check) {
        return Err(Failure("a2_doctor_failed"));
    }
    Ok(version)
}

async fn resume(adb: &Adb, input: &Input, requested: Option<&str>) -> Result<String> {
    let installed = owner_version(adb).await?;
    let expected = match input {
        Input::Offline(bundle) => Some(bundle.version.as_str()),
        Input::Remote(_) => requested,
    };
    if expected.is_some_and(|expected| expected != installed) {
        return Err(Failure("a2_version_mismatch"));
    }
    Ok(installed)
}

async fn resume_supervisor(adb: &Adb) -> Result<()> {
    if adb.script("start").await?.0 {
        Ok(())
    } else {
        Err(Failure("setup_account_unsettled"))
    }
}

async fn activate(
    adb: &Adb,
    scratch: &Scratch,
    (adapter, input): (&Path, &Input),
    config: Option<&Path>,
    policy: Option<Vec<u8>>,
) -> Result<String> {
    let policy = policy.ok_or(Failure("magiskpolicy_download_failed"))?;
    let local_tool = scratch.0.join("magiskpolicy");
    fs::write(&local_tool, policy).map_err(|_| Failure("temporary_files_failed"))?;
    let name = scratch
        .0
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(Failure("temporary_files_failed"))?;
    let stage = format!("{LOCAL}/{name}");
    adb.shell(&format!("set -e; {PROTECTED_LOCAL}; umask 077; mkdir '{stage}'"))
        .await
        .map_err(|_| Failure("a2_stage_failed"))?;
    let result = prepare_install(adb, scratch, (adapter, input), &stage, (&local_tool, config)).await;
    clean_stage(adb, &stage).await.and(result)
}

async fn prepare_install(
    adb: &Adb,
    scratch: &Scratch,
    (adapter, input): (&Path, &Input),
    stage: &str,
    (tool, config): (&Path, Option<&Path>),
) -> Result<String> {
    let version = stage_bundle(adb, scratch, stage, adapter, input).await?;
    adb.push(tool, &format!("{stage}/magiskpolicy")).await?;
    if let Some(config) = config {
        adb.push(config, &format!("{stage}/bundle/.host-config")).await?;
    }
    adb.shell(&format!(
        "set -e; umask 022; {PROTECTED_LOCAL}; \
        for p in '{ROOT}' '{ROOT}/bin'; do if test ! -e \"$p\" && test ! -L \"$p\"; then mkdir \"$p\"; fi; \
        test ! -L \"$p\" && test -d \"$p\" && test \"$(stat -c %u:%g:%a \"$p\")\" = 0:0:755 || exit 1; done"
    ))
    .await
    .map_err(|_| Failure("a2_stage_failed"))?;
    run_installer(adb, stage).await?;
    Ok(version)
}

async fn run_installer(adb: &Adb, stage: &str) -> Result<()> {
    let bundle = format!("{stage}/bundle");
    let output = adb
        .shell(&format!(
            "set -e; chmod 700 '{stage}/magiskpolicy'; chmod 755 '{bundle}/bin/echo-bridge'; \
        '{bundle}/bin/echo-bridge' install --magiskpolicy '{stage}/magiskpolicy' --stage '{bundle}' --host-stage --no-reboot"
        ))
        .await
        .map_err(|_| Failure("a2_install_failed"))?;
    if output == "echo-bridge install: installed\n" {
        return Ok(());
    }
    Err(Failure("a2_install_failed"))
}

async fn boot_id(adb: &Adb) -> Result<String> {
    let value = adb.shell(BOOT_ID).await?;
    let id = value.trim();
    if id.len() != 36 || !id.bytes().all(|byte| byte.is_ascii_hexdigit() || byte == b'-') {
        return Err(Failure("a2_boot_unconfirmed"));
    }
    Ok(id.to_owned())
}

async fn rebooted(adb: &Adb, before: &str, deadline: Instant) -> Result<()> {
    tokio::time::timeout_at(deadline, async {
        loop {
            if boot_id(adb).await.is_ok_and(|id| id != before)
                && adb
                    .shell("getprop sys.boot_completed")
                    .await
                    .is_ok_and(|value| value == "1\n")
            {
                return;
            }
            tokio::time::sleep(WAIT).await;
        }
    })
    .await
    .map_err(|_| Failure("a2_boot_timeout"))
}

async fn owner_version(adb: &Adb) -> Result<String> {
    let (success, output) = adb.script("owner").await?;
    let version = output.trim_end_matches('\n');
    if success && crate::version::normalized(version).as_deref() == Some(version) {
        Ok(version.to_owned())
    } else {
        Err(Failure("a2_owner_unverified"))
    }
}

pub(crate) async fn started(adb: &Adb, version: &str, deadline: Instant) -> Result<()> {
    tokio::time::timeout_at(deadline, async {
        loop {
            match crate::lifecycle::require_started(adb, Some(version)).await {
                Err(Failure("start_failed")) => tokio::time::sleep(WAIT).await,
                outcome => return outcome,
            }
        }
    })
    .await
    .map_err(|_| Failure("a2_boot_timeout"))?
}

async fn configure_private(
    adb: &Adb,
    options: &Options,
    key: Option<&Secret<String>>,
    deadline: Instant,
) -> Result<()> {
    crate::setup::stop(adb, deadline).await?;
    if options.force {
        let path = options.config.as_deref().ok_or(Failure("config_unreadable"))?;
        let config = fs::read(path).map_err(|_| Failure("config_unreadable"))?;
        adb.shell_input(&crate::adb::a2_command("config-write"), &config)
            .await
            .map_err(|_| Failure("setup_config_invalid"))?;
    }
    if let Some(key) = key {
        let connection = crate::setup::InstalledConnection::selected(adb.clone(), deadline, None, None).await?;
        connection.install_api_key("", key).await?;
    }
    resume_supervisor(adb).await
}
