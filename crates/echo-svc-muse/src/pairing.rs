use std::{
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use echo_contracts::secret::Secret;
use reqwest::Url;
use ring::rand::{SecureRandom, SystemRandom};
use rustix::{
    fs::{AtFlags, CWD, Mode, OFlags, fchmod, openat, renameat, unlinkat},
    io::Errno,
    process::geteuid,
};
use serde::{Deserialize, Serialize, Serializer};
use x25519_dalek::StaticSecret;

use crate::ServiceError;

const FILE_BYTES: u64 = 64 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    #[serde(serialize_with = "serialize_secret")]
    node_id: Secret<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    noise_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_url_v2: Option<String>,
    #[serde(serialize_with = "serialize_secret")]
    access_token: Secret<String>,
    #[serde(serialize_with = "serialize_secret")]
    refresh_token: Secret<String>,
    access_token_saved_at: u64,
    #[serde(serialize_with = "serialize_secret")]
    noise_static_key: Secret<String>,
}

#[derive(Deserialize)]
pub(super) struct Tokens {
    access_token: Secret<String>,
    refresh_token: Secret<String>,
}

impl Tokens {
    pub(super) fn valid(&self) -> bool {
        valid_token(self.access_token.expose()) && refresh_value(self.refresh_token.expose()).is_some()
    }
}

pub(crate) struct Pairing {
    pub(super) noise_url: Url,
    pub(super) api_url: Url,
    pub(super) static_key: Secret<StaticSecret>,
    record: Record,
    directory: File,
    name: PathBuf,
    _lease: File,
}

impl Pairing {
    pub(super) fn open(path: &Path) -> Result<Self, ServiceError> {
        let name = PathBuf::from(path.file_name().ok_or(ServiceError::UnsafePairing)?);
        let directory = open_directory(path.parent().ok_or(ServiceError::UnsafePairing)?)?;
        let metadata = directory.metadata().map_err(|_| ServiceError::UnsafePairing)?;
        if metadata.uid() != geteuid().as_raw() || metadata.mode() & 0o022 != 0 {
            return Err(ServiceError::UnsafePairing);
        }
        let file = openat(&directory, &name, private_flags() | OFlags::RDONLY, Mode::empty())
            .map(File::from)
            .map_err(file_error)?;
        check_file(&file)?;
        let lease = lock(&directory, &name)?;
        // Reopen under the lock to observe any prior owner's atomic replacement.
        let file = openat(&directory, &name, private_flags() | OFlags::RDONLY, Mode::empty())
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
        Self::from_record(record, directory, name, lease)
    }

    fn from_record(record: Record, directory: File, name: PathBuf, lease: File) -> Result<Self, ServiceError> {
        if record.version != 1
            || record.node_id.expose().trim().is_empty()
            || !valid_token(record.access_token.expose())
            || refresh_value(record.refresh_token.expose()).is_none()
        {
            return Err(ServiceError::UnsafePairing);
        }
        let static_bytes = STANDARD
            .decode(record.noise_static_key.expose())
            .ok()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .ok_or(ServiceError::UnsafePairing)?;
        let mut noise_url = parse_url(
            &format!("wss://{}", record.noise_host.as_deref().unwrap_or("hatch.metaaivm.com")),
            "wss",
        )?;
        if noise_url.path() != "/" {
            return Err(ServiceError::UnsafePairing);
        }
        noise_url.set_path("/v1/noise");
        let api_url = parse_url(record.api_url_v2.as_deref().unwrap_or("https://api.muse.ai"), "https")?;
        Ok(Self {
            noise_url,
            api_url,
            static_key: Secret::new(StaticSecret::from(static_bytes)),
            record,
            directory,
            name,
            _lease: lease,
        })
    }

    pub(super) fn node_id(&self) -> &Secret<String> {
        &self.record.node_id
    }

    pub(super) fn access_token(&self) -> &str {
        self.record.access_token.expose()
    }

    pub(super) fn refresh_token(&self) -> &str {
        self.record.refresh_token.expose()
    }

    pub(super) fn saved_at(&self) -> u64 {
        self.record.access_token_saved_at
    }

    pub(super) fn replace_tokens(&mut self, tokens: Tokens, saved_at: u64) -> Result<(), ServiceError> {
        let record = Record {
            version: self.record.version,
            node_id: Secret::new(self.record.node_id.expose().clone()),
            noise_host: self.record.noise_host.clone(),
            api_url_v2: self.record.api_url_v2.clone(),
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            access_token_saved_at: saved_at,
            noise_static_key: Secret::new(self.record.noise_static_key.expose().clone()),
        };
        self.persist(&record)?;
        self.record = record;
        Ok(())
    }

    fn persist(&self, record: &Record) -> Result<(), ServiceError> {
        let bytes = serde_json::to_vec(record).map_err(|_| ServiceError::Storage)?;
        if bytes.len() as u64 > FILE_BYTES {
            return Err(ServiceError::Storage);
        }
        let mut suffix = [0; 16];
        SystemRandom::new()
            .fill(&mut suffix)
            .map_err(|_| ServiceError::Storage)?;
        let staging = format!(".muse-pairing-{}", hex::encode(suffix));
        let flags = private_flags() | OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL;
        let mode = Mode::RUSR | Mode::WUSR;
        let mut file = File::from(openat(&self.directory, &staging, flags, mode).map_err(|_| ServiceError::Storage)?);
        let result = (|| {
            fchmod(&file, mode).map_err(|_| ServiceError::Storage)?;
            file.write_all(&bytes).map_err(|_| ServiceError::Storage)?;
            file.sync_all().map_err(|_| ServiceError::Storage)?;
            renameat(&self.directory, &staging, &self.directory, &self.name).map_err(|_| ServiceError::Storage)
        })();
        if result.is_err() {
            let _ = unlinkat(&self.directory, staging, AtFlags::empty());
        }
        result?;
        self.directory.sync_all().map_err(|_| ServiceError::Storage)
    }

    pub(super) fn remove(&self) -> Result<(), ServiceError> {
        match unlinkat(&self.directory, &self.name, AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => self.directory.sync_all().map_err(|_| ServiceError::Storage),
            Err(_) => Err(ServiceError::Storage),
        }
    }
}

fn serialize_secret<S: Serializer>(secret: &Secret<String>, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(secret.expose())
}

pub(super) fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_graphic())
}

pub(super) fn refresh_value(token: &str) -> Option<&str> {
    let value = token.rsplit(':').next()?;
    (valid_token(token) && !value.is_empty()).then_some(value)
}

pub(super) fn parse_url(value: &str, scheme: &str) -> Result<Url, ServiceError> {
    if value
        .bytes()
        .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control() || byte == b'\\')
    {
        return Err(ServiceError::UnsafePairing);
    }
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
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    openat(&dir, ".", flags, Mode::empty())
        .map(File::from)
        .map_err(file_error)
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
        assert_eq!(admitted.node_id().expose(), "gadget-fixture");
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
        assert_eq!(Pairing::open(&path).unwrap().node_id().expose(), "gadget-fixture");
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
    fn t1_auth_failed_rename_keeps_old_credentials_and_removes_staging() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("pairing.json");
        let saved = root.path().join("before.json");
        write_pairing(&path, &pairing_record());
        let original = std::fs::read(&path).unwrap();
        let mut pairing = Pairing::open(&path).unwrap();
        std::fs::rename(&path, &saved).unwrap();
        std::fs::create_dir(&path).unwrap();
        let tokens = Tokens {
            access_token: Secret::new("synthetic-replacement-access".into()),
            refresh_token: Secret::new("synthetic-replacement-refresh".into()),
        };
        assert_eq!(
            pairing.replace_tokens(tokens, 1_700_010_800),
            Err(ServiceError::Storage)
        );
        assert_eq!(pairing.access_token(), "synthetic-access");
        assert_eq!(pairing.refresh_token(), "synthetic-refresh");
        assert_eq!(pairing.saved_at(), 1_700_000_000);
        assert_eq!(std::fs::read(&saved).unwrap(), original);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 3);
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
            ("refresh_token", serde_json::json!("hatch_refresh:")),
            ("noise_static_key", serde_json::json!("short")),
            ("noise_host", serde_json::json!("127.0.0.1/path")),
            ("api_url_v2", serde_json::json!("http://127.0.0.1")),
            ("api_url_v2", serde_json::json!("https://127.0.0.1?bad")),
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
