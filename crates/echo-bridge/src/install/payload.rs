use std::{
    collections::BTreeMap,
    fs::File,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use echo_boot::{Digest, Result, layout};
use serde::Deserialize;

use super::files::{self, CORE, Files, ROOT, read_at, read_text, source_dir};

const LEGACY: &str = "/data/local/echo-bridge";
const CONFIG_LIMIT: u64 = 1024 * 1024;
const CANDIDATE: &str = "bin/echo-bridge";
const DEFAULT_CONFIG: &str = "examples/bridge.echo.toml";
pub(super) const ASSETS: [(&str, &str, u32); 3] = [
    ("bin/codex-app-server", "codex-app-server", 0o755),
    ("bin/cacert.pem", "cacert.pem", 0o644),
    ("apk/echo-screen.apk", layout::SCREEN_APK, 0o644),
];

#[derive(Deserialize)]
struct Manifest {
    format: u32,
    version: String,
    files: BTreeMap<String, String>,
}

pub(super) struct Release {
    pub version: String,
    stage: PathBuf,
}

impl Release {
    pub(super) fn verify(files: &Files<'_>, stage: &Path, next: &Path) -> Result<Self> {
        let manifest = read_text(files.trusted_file(&stage.join("MANIFEST"))?, 64 * 1024)?;
        let manifest: Manifest = serde_json::from_str(&manifest).map_err(|_| "invalid install manifest")?;
        if manifest.format != 1 || !valid_version(&manifest.version) {
            return Err("invalid install manifest");
        }
        for (name, path) in ASSETS.iter().map(|(name, _, _)| (*name, stage.join(name))).chain([
            (DEFAULT_CONFIG, stage.join(DEFAULT_CONFIG)),
            (CANDIDATE, next.to_path_buf()),
        ]) {
            let expected = manifest.files.get(name).ok_or("install manifest member missing")?;
            Digest::parse(expected.clone())?;
            let file = files.trusted_file(&path)?;
            if file.metadata().map_err(|_| "inspect release member")?.len() == 0 || files::digest(file)? != *expected {
                return Err("install manifest checksum mismatch");
            }
        }
        if files
            .trusted_file(next)?
            .metadata()
            .map_err(|_| "inspect staged candidate")?
            .mode()
            & 0o7777
            != 0o755
        {
            return Err("invalid staged candidate permissions");
        }
        Ok(Self {
            version: manifest.version,
            stage: stage.to_path_buf(),
        })
    }

    pub(super) fn prepare_slots(&self, files: &Files<'_>, work: &Path, next: &Path) -> Result<()> {
        for (source, name, mode) in ASSETS {
            files.copy(
                files.trusted_file(&self.stage.join(source))?,
                &work.join(name),
                ROOT,
                mode,
            )?;
        }
        files.copy(files.trusted_file(next)?, &work.join("boot"), ROOT, 0o755)
    }

    pub(super) fn default_config(&self, files: &Files<'_>) -> Result<String> {
        read_text(files.trusted_file(&self.stage.join(DEFAULT_CONFIG))?, CONFIG_LIMIT)
    }
}

pub(super) fn valid_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 64
        && version.bytes().next().is_some_and(|byte| byte.is_ascii_digit())
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte))
}

pub(super) fn prepare_private(files: &Files<'_>, release: &Release, work: &Path, target: &Path) -> Result<()> {
    if files::exists(target)? {
        return Ok(());
    }
    let private = work.join(layout::PRIVATE);
    files.directory(&private, CORE, 0o700)?;
    let source = legacy_dir(files, layout::PRIVATE)?;
    if let Some(source) = &source {
        migrate_private(files, source, &private)?;
    }
    let config = match &source {
        Some(source) if present(source, Path::new(layout::CONFIG))? => {
            read_text(read_at(source, Path::new(layout::CONFIG))?, CONFIG_LIMIT)?
        }
        _ => release.default_config(files)?,
    };
    files.write(
        &private.join(layout::CONFIG),
        migrate_config(&config)?.as_bytes(),
        CORE,
        0o600,
    )?;
    files.directory(&private.join(layout::RUN), CORE, 0o700)
}

pub(super) fn prepare_home(files: &Files<'_>, work: &Path, target: &Path) -> Result<()> {
    if files::exists(target)? {
        return check_private_dir(files, target).map(|_| ());
    }
    let prepared = work.join(layout::HOME);
    match legacy_dir(files, layout::HOME)? {
        Some(source) => files::copy_tree(files, &source, &prepared, CORE, 0o700, 32),
        None => files.directory(&prepared, CORE, 0o700),
    }
}

fn legacy_dir(files: &Files<'_>, name: &str) -> Result<Option<File>> {
    let root = files.path(LEGACY);
    if !files::exists(&root)? {
        return Ok(None);
    }
    let legacy = files.protected(&root)?;
    Ok(if present(&legacy, Path::new(name))? {
        Some(check_private_dir(files, &root.join(name))?)
    } else {
        None
    })
}

fn check_private_dir(files: &Files<'_>, path: &Path) -> Result<File> {
    let dir = files::open_dir(path)?;
    let meta = dir.metadata().map_err(|_| "inspect private data")?;
    let expected_uid = if files.root_uid == 0 { CORE.0 } else { files.root_uid };
    if meta.uid() != expected_uid || files::mode(&meta) != 0o700 {
        return Err("unsafe private data directory");
    }
    Ok(dir)
}

pub(super) fn validate_retained_private(files: &Files<'_>, target: &Path) -> Result<()> {
    let private = check_private_dir(files, target)?;
    let config = read_text(read_at(&private, Path::new(layout::CONFIG))?, CONFIG_LIMIT)?;
    check_private_dir(files, &target.join(layout::RUN))?;
    let migrated = migrate_config(&config)?;
    let observed: toml::Table = config.parse().map_err(|_| "invalid install configuration")?;
    let expected: toml::Table = migrated.parse().map_err(|_| "invalid install configuration")?;
    if observed != expected {
        return Err("conflicting retained private configuration");
    }
    Ok(())
}

pub(super) fn validate_retained_home(files: &Files<'_>, target: &Path) -> Result<()> {
    check_private_dir(files, target).map(|_| ())
}

fn migrate_private(files: &Files<'_>, source: &File, target: &Path) -> Result<()> {
    let entries = rustix::fs::Dir::read_from(source).map_err(|_| "read legacy private data")?;
    for entry in entries {
        let entry = entry.map_err(|_| "read legacy private entry")?;
        let name = entry.file_name().to_bytes();
        if matches!(
            name,
            b"." | b".." | b"bridge.toml" | b"run" | b"bridge.pid" | b"bridge.trace" | b"stop" | b"finished"
        ) {
            continue;
        }
        let name = Path::new(std::os::unix::ffi::OsStrExt::from_bytes(name));
        let target = target.join(name);
        if entry.file_type() == rustix::fs::FileType::Directory {
            files::copy_tree(files, &source_dir(source, name)?, &target, CORE, 0o700, 32)?;
        } else {
            files.copy(read_at(source, name)?, &target, CORE, 0o600)?;
        }
    }
    Ok(())
}

fn present(dir: &File, name: &Path) -> Result<bool> {
    match rustix::fs::statat(dir, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(true),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(_) => Err("inspect legacy data path"),
    }
}

fn migrate_config(source: &str) -> Result<String> {
    let mut table: toml::Table = source.parse().map_err(|_| "invalid install configuration")?;
    for (section, key, path) in [
        ("live", "credential_file", "private/live.json"),
        ("codex", "home", "private/codex-home"),
        ("codex", "binary", "bin/codex-app-server"),
        ("codex", "ca_file", "bin/cacert.pem"),
        ("mcp", "state_dir", "private/portos"),
        ("screen", "pairing_token_file", "private/screen-pairing.txt"),
        ("audit", "log_dir", "private/audit"),
        ("home", "dir", "home"),
    ] {
        if let Some(value) = table
            .get_mut(section)
            .and_then(toml::Value::as_table_mut)
            .and_then(|part| part.get_mut(key))
            && let Some(old) = value
                .as_str()
                .and_then(|old| old.strip_prefix("/data/local/echo-bridge/"))
            && (old == path || (section == "codex" && path.strip_prefix("bin/") == Some(old)))
        {
            *value = toml::Value::String(format!("{}/{path}", layout::ROOT));
        }
    }
    let encoded = toml::to_string(&table).map_err(|_| "encode install configuration")?;
    let config = echo_config::BridgeConfig::from_toml_str(&encoded).map_err(|_| "invalid install configuration")?;
    if config.echo.host != "127.0.0.1" {
        return Err("A2 configuration requires the local satellite");
    }
    Ok(encoded)
}
