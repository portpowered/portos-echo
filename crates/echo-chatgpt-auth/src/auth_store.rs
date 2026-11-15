use crate::{AuthError, ChatGptCredential, auth_private, auth_token};
use echo_contracts::secret::Secret;
use serde::{Deserialize, Serialize, Serializer, ser::SerializeStruct};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub(crate) access: Secret<String>,
    pub(crate) refresh: Secret<String>,
    pub(crate) expires: u64,
    pub(crate) account_id: String,
    pub(crate) last_refresh: u64,
}
impl Serialize for Record {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut record = serializer.serialize_struct("Credential", 5)?;
        record.serialize_field("access", self.access.expose())?;
        record.serialize_field("refresh", self.refresh.expose())?;
        record.serialize_field("expires", &self.expires)?;
        record.serialize_field("account_id", &self.account_id)?;
        record.serialize_field("last_refresh", &self.last_refresh)?;
        record.end()
    }
}
impl Record {
    pub(crate) fn due(&self, now: u64) -> bool {
        self.expires.saturating_sub(now) <= 300 || now.saturating_sub(self.last_refresh) > 691_200
    }
    pub(crate) fn credential(self) -> ChatGptCredential {
        ChatGptCredential {
            access_token: self.access,
            account_id: self.account_id,
        }
    }
    fn validate(&self) -> Result<(), AuthError> {
        let (account, exp) = auth_token::claims(self.access.expose())?;
        auth_token::nonempty(self.refresh.expose())?;
        if account != self.account_id {
            return Err(AuthError::AccountMismatch);
        }
        if self.expires == 0 || self.expires > exp {
            return Err(AuthError::InvalidCredential);
        }
        Ok(())
    }
}
pub(crate) fn load(path: &Path) -> Result<Record, AuthError> {
    let file = File::open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AuthError::NotSignedIn
        } else {
            AuthError::Storage
        }
    })?;
    let mut bytes = Vec::new();
    file.take(262_145)
        .read_to_end(&mut bytes)
        .map_err(|_| AuthError::Storage)?;
    if bytes.len() > 262_144 {
        return Err(AuthError::InvalidCredential);
    }
    let record: Record = serde_json::from_slice(&bytes).map_err(|_| AuthError::InvalidCredential)?;
    record.validate()?;
    Ok(record)
}
pub(crate) fn save(path: &Path, record: &Record) -> Result<(), AuthError> {
    record.validate()?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let directory = auth_private::Directory::new(parent)?;
    let temporary = directory.0.join("credential");
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(|_| AuthError::Storage)?;
    serde_json::to_writer(&mut file, record).map_err(|_| AuthError::Storage)?;
    file.flush()
        .and_then(|()| file.sync_all())
        .map_err(|_| AuthError::Storage)?;
    drop(file);
    fs::rename(temporary, path).map_err(|_| AuthError::Storage)?;
    Ok(())
}
