//! Release integrity checks happen before adb discovery, including offline input.
use crate::{Failure, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};

pub const REQUIRED: &[&str] = &[
    "bin/echo-bridge",
    "bin/codex-app-server",
    "bin/cacert.pem",
    "apk/echo-screen.apk",
    "scripts/echo-install.sh",
    "scripts/echo-install-release.sh",
    "examples/bridge.echo.toml",
    "examples/prompts/voice.md",
    "examples/prompts/runner.md",
    "LICENSE",
    "NOTICE",
    "licenses/codex/LICENSE",
    "licenses/codex/NOTICE",
    "licenses/ca/MPL-2.0.txt",
    "licenses/ca/NOTICE",
];

#[must_use]
pub fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut result, byte| {
            let _ = write!(result, "{byte:02x}");
            result
        })
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Require one exact, strict selected entry; no basename or substring matching.
/// # Errors
/// Rejects missing, malformed or duplicated entries with a fixed category.
pub fn selected_checksum(sums: &str, name: &str) -> Result<String> {
    let mut selected = None;
    for line in sums.lines() {
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts
            .get(1)
            .is_some_and(|entry| *entry == name || entry.strip_prefix('*') == Some(name))
        {
            let expected = format!("{}  {name}", parts[0]);
            if selected.is_some() || !valid_digest(parts[0]) || line != expected {
                return Err(Failure("invalid_checksum"));
            }
            selected = Some(parts[0].to_owned());
        }
    }
    selected.ok_or(Failure("invalid_checksum"))
}

#[derive(Deserialize)]
struct Manifest {
    format: u32,
    version: String,
    files: BTreeMap<String, String>,
}

pub struct OfflineBundle {
    pub root: PathBuf,
    pub version: String,
    pub files: Vec<String>,
}

/// Validate the entire tree before any device effect; never traverse links.
/// # Errors
/// Rejects unsafe members, missing files, unsupported manifests and wrong hashes.
pub fn offline(directory: &Path) -> Result<OfflineBundle> {
    let root = directory.canonicalize().map_err(|_| Failure("invalid_bundle"))?;
    let manifest_path = root.join("MANIFEST");
    regular(&manifest_path)?;
    let encoded = fs::read(&manifest_path).map_err(|_| Failure("invalid_bundle"))?;
    let manifest: Manifest = serde_json::from_slice(&encoded).map_err(|_| Failure("invalid_bundle"))?;
    if manifest.format != 1 || !REQUIRED.iter().all(|name| manifest.files.contains_key(*name)) {
        return Err(Failure("invalid_bundle"));
    }
    let version = crate::cli::version(&manifest.version)?;
    if version != manifest.version {
        return Err(Failure("invalid_bundle"));
    }
    let mut observed = BTreeMap::new();
    inspect(&root, &root, &mut observed)?;
    observed.remove("MANIFEST");
    if observed != manifest.files {
        return Err(Failure("manifest_mismatch"));
    }
    let mut files: Vec<_> = observed.into_keys().collect();
    files.push("MANIFEST".to_owned());
    Ok(OfflineBundle { root, version, files })
}

fn regular(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path)
        .map_err(|_| Failure("invalid_bundle"))?
        .file_type()
        .is_file()
    {
        return Err(Failure("unsafe_bundle_member"));
    }
    Ok(())
}

fn inspect(root: &Path, directory: &Path, files: &mut BTreeMap<String, String>) -> Result<()> {
    for entry in fs::read_dir(directory).map_err(|_| Failure("invalid_bundle"))? {
        let entry = entry.map_err(|_| Failure("invalid_bundle"))?;
        let path = entry.path();
        let name = path
            .strip_prefix(root)
            .map_err(|_| Failure("invalid_bundle"))?
            .to_str()
            .ok_or(Failure("unsafe_bundle_member"))?
            .replace('\\', "/");
        if !safe_name(&name) {
            return Err(Failure("unsafe_bundle_member"));
        }
        let kind = entry.file_type().map_err(|_| Failure("invalid_bundle"))?;
        if kind.is_dir() {
            inspect(root, &path, files)?;
        } else {
            regular(&path)?;
            if !allowed_file(&name) {
                return Err(Failure("unsafe_bundle_member"));
            }
            files.insert(name, digest(&fs::read(path).map_err(|_| Failure("invalid_bundle"))?));
        }
    }
    Ok(())
}

#[must_use]
pub fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || b"_./-".contains(&c))
}

fn allowed_file(name: &str) -> bool {
    name == "MANIFEST" || REQUIRED.contains(&name) || name.starts_with("licenses/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_lf_and_crlf_match_and_duplicates_fail() -> Result<()> {
        let hash = digest(b"release");
        for ending in ["\n", "\r\n"] {
            let line = format!("{hash}  echo-0.2.0-armv7.tar.gz{ending}");
            assert_eq!(selected_checksum(&line, "echo-0.2.0-armv7.tar.gz")?, hash);
            assert_eq!(
                selected_checksum(&line.repeat(2), "echo-0.2.0-armv7.tar.gz"),
                Err(Failure("invalid_checksum"))
            );
        }
        Ok(())
    }

    #[test]
    fn checksum_is_strict_about_selected_entry() {
        let hash = digest(b"release");
        for sums in [
            format!("{hash} *asset"),
            format!("{hash} asset"),
            format!("{hash}  asset extra"),
            format!("{}  asset", hash.to_uppercase()),
            "bad  asset".to_owned(),
            format!("{hash}  other"),
        ] {
            assert_eq!(selected_checksum(&sums, "asset"), Err(Failure("invalid_checksum")));
        }
    }

    #[test]
    fn member_paths_reject_shell_and_traversal_syntax() {
        for name in ["../bin/x", "/bin/x", "a//b", "a/./b", "a/../b", "a\\b", "a;id", "a\nb"] {
            assert!(!safe_name(name));
        }
        assert!(safe_name("licenses/echo/0123/LICENSE-MIT"));
    }
}
