use std::{
    fs::File,
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Component, Path},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use echo_contracts::secret::Secret;
use reqwest::Url;
use rustix::{
    fs::{CWD, Mode, OFlags, openat},
    io::Errno,
    process::geteuid,
};
use serde::Deserialize;
use x25519_dalek::StaticSecret;

use crate::ServiceError;

const FILE_BYTES: u64 = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    node_id: Secret<String>,
    noise_host: Option<String>,
    api_url_v2: Option<String>,
    access_token: Secret<String>,
    refresh_token: Secret<String>,
    #[serde(rename = "access_token_saved_at")]
    _access_token_saved_at: u64,
    noise_static_key: Secret<String>,
}

pub(crate) struct Pairing {
    pub(super) node_id: Secret<String>,
    pub(super) noise_url: Url,
    pub(super) api_url: Url,
    pub(super) access_token: Secret<String>,
    pub(super) static_key: Secret<StaticSecret>,
    _lease: File,
}

impl Pairing {
    pub(super) fn open(path: &Path) -> Result<Self, ServiceError> {
        let name = path.file_name().ok_or(ServiceError::UnsafePairing)?;
        let parent = open_directory(path.parent().ok_or(ServiceError::UnsafePairing)?)?;
        let metadata = parent.metadata().map_err(|_| ServiceError::UnsafePairing)?;
        if metadata.uid() != geteuid().as_raw() || metadata.mode() & 0o022 != 0 {
            return Err(ServiceError::UnsafePairing);
        }
        let file = openat(&parent, name, private_flags() | OFlags::RDONLY, Mode::empty())
            .map(File::from)
            .map_err(file_error)?;
        check_file(&file)?;
        let lease = lock(&parent, Path::new(name))?;
        // Reopen under the lock to observe any prior owner's atomic replacement.
        let file = openat(&parent, name, private_flags() | OFlags::RDONLY, Mode::empty())
            .map(File::from)
            .map_err(file_error)?;
        check_file(&file)?;
        let mut bytes = Vec::new();
        file.take(FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ServiceError::UnsafePairing)?;
        if bytes.len() as u64 > FILE_BYTES {
            return Err(ServiceError::UnsafePairing);
        }
        let record = serde_json::from_slice(&bytes).map_err(|_| ServiceError::UnsafePairing)?;
        Self::from_record(record, lease)
    }

    fn from_record(record: Record, lease: File) -> Result<Self, ServiceError> {
        let Record {
            version,
            node_id,
            noise_host,
            api_url_v2,
            access_token,
            refresh_token,
            _access_token_saved_at: _,
            noise_static_key,
        } = record;
        if version != 1
            || node_id.expose().trim().is_empty()
            || !valid_token(access_token.expose())
            || !valid_token(refresh_token.expose())
        {
            return Err(ServiceError::UnsafePairing);
        }
        let static_bytes = STANDARD
            .decode(noise_static_key.expose())
            .ok()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .ok_or(ServiceError::UnsafePairing)?;
        let mut noise_url = parse_url(
            &format!("wss://{}", noise_host.as_deref().unwrap_or("hatch.metaaivm.com")),
            "wss",
        )?;
        if noise_url.path() != "/" {
            return Err(ServiceError::UnsafePairing);
        }
        noise_url.set_path("/v1/noise");
        let api_url = parse_url(api_url_v2.as_deref().unwrap_or("https://api.muse.ai"), "https")?;
        Ok(Self {
            node_id,
            noise_url,
            api_url,
            access_token,
            static_key: Secret::new(StaticSecret::from(static_bytes)),
            _lease: lease,
        })
    }
}

pub(super) fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_graphic())
}

pub(super) fn parse_url(value: &str, scheme: &str) -> Result<Url, ServiceError> {
    let url = Url::parse(value).map_err(|_| ServiceError::UnsafePairing)?;
    if url.scheme() != scheme
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ServiceError::UnsafePairing);
    }
    Ok(url)
}

fn private_flags() -> OFlags {
    OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
}

fn open_directory(path: &Path) -> Result<File, ServiceError> {
    let flags = OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let base = if path.is_absolute() { "/" } else { "." };
    let mut dir = openat(CWD, base, flags, Mode::empty())
        .map(File::from)
        .map_err(file_error)?;
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                dir = openat(&dir, name, flags, Mode::empty())
                    .map(File::from)
                    .map_err(file_error)?;
            }
            _ => return Err(ServiceError::UnsafePairing),
        }
    }
    Ok(dir)
}

fn lock(dir: &File, name: &Path) -> Result<File, ServiceError> {
    let path = name.with_added_extension("lock");
    let flags = private_flags() | OFlags::RDWR | OFlags::CREATE;
    let file = openat(dir, path, flags, Mode::RUSR | Mode::WUSR)
        .map(File::from)
        .map_err(|_| ServiceError::UnsafePairing)?;
    check_file(&file)?;
    file.try_lock().map_err(|_| ServiceError::UnsafePairing)?;
    Ok(file)
}

fn check_file(file: &File) -> Result<(), ServiceError> {
    let metadata = file.metadata().map_err(|_| ServiceError::UnsafePairing)?;
    check_metadata(&metadata, geteuid().as_raw())
}

fn check_metadata(metadata: &std::fs::Metadata, owner: u32) -> Result<(), ServiceError> {
    if !metadata.is_file()
        || metadata.mode() & 0o7777 != 0o600
        || metadata.uid() != owner
        || metadata.nlink() != 1
        || metadata.len() > FILE_BYTES
    {
        return Err(ServiceError::UnsafePairing);
    }
    Ok(())
}

fn file_error(error: Errno) -> ServiceError {
    if error == Errno::NOENT {
        ServiceError::PairingUnavailable
    } else {
        ServiceError::UnsafePairing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{pairing_record, write_pairing};
    use std::os::unix::fs::symlink;

    #[test]
    fn t1_pairing_admits_owner_and_excludes_second_holder() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("pairing.json");
        write_pairing(&path, &pairing_record());
        let admitted = Pairing::open(&path).unwrap();
        assert_eq!(admitted.node_id.expose(), "gadget-fixture");
        assert_eq!(admitted.static_key.expose().to_bytes(), [7; 32]);
        assert_eq!(admitted.api_url.host_str(), Some("api.muse.ai"));
        assert_eq!(admitted.noise_url.path(), "/v1/noise");
        assert!(matches!(Pairing::open(&path), Err(ServiceError::UnsafePairing)));
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(
            check_metadata(&meta, meta.uid().wrapping_add(1)),
            Err(ServiceError::UnsafePairing)
        );
        drop(admitted);
        assert_eq!(Pairing::open(&path).unwrap().node_id.expose(), "gadget-fixture");
    }

    #[test]
    fn t1_pairing_refuses_missing_linked_or_nonregular_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("pairing.json");
        let link = root.path().join("linked.json");
        assert!(matches!(Pairing::open(&path), Err(ServiceError::PairingUnavailable)));
        write_pairing(&path, &pairing_record());
        symlink(&path, &link).unwrap();
        assert!(matches!(Pairing::open(&link), Err(ServiceError::UnsafePairing)));
        assert!(matches!(Pairing::open(root.path()), Err(ServiceError::UnsafePairing)));
        std::fs::remove_file(&link).unwrap();
        std::fs::hard_link(&path, &link).unwrap();
        assert!(matches!(Pairing::open(&path), Err(ServiceError::UnsafePairing)));
    }

    #[test]
    fn t1_pairing_refuses_invalid_fields() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("pairing.json");
        for (key, value) in [
            ("version", serde_json::json!(2)),
            ("access_token_saved_at", serde_json::json!(-1)),
            ("node_id", serde_json::json!("")),
            ("access_token", serde_json::json!("token\r\n")),
            ("refresh_token", serde_json::json!("")),
            ("noise_static_key", serde_json::json!("short")),
            ("noise_host", serde_json::json!("fixture.example.invalid/path")),
            ("api_url_v2", serde_json::json!("http://fixture.example.invalid")),
            ("api_url_v2", serde_json::json!("https://fixture.example.invalid?bad")),
        ] {
            let mut record = pairing_record();
            record[key] = value;
            write_pairing(&path, &record);
            assert!(
                matches!(Pairing::open(&path), Err(ServiceError::UnsafePairing)),
                "{key}"
            );
        }
    }
}
