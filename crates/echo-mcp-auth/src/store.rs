use crate::{
    AuthError, AuthOptions, LinkClient,
    proof::{self, PrivateJwk},
};
use echo_contracts::secret::Secret;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub version: u8,
    pub issuer: String,
    pub resource: String,
    pub client_id: Option<String>,
    #[serde(default)]
    pub registration: Option<Registration>,
    #[serde(deserialize_with = "load_key")]
    pub private_key_jwk: Arc<Secret<PrivateJwk>>,
    pub grant: Option<Secret<Grant>>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Registration {
    pub name: String,
    pub scope: String,
}

fn load_key<'de, D: serde::Deserializer<'de>>(input: D) -> Result<Arc<Secret<PrivateJwk>>, D::Error> {
    Secret::deserialize(input).map(Arc::new)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Grant {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_unix: u64,
    pub scope: String,
    /// The RFC 8707 resources the owner approved when the sign-in named more
    /// than one. Empty for a grant approved for the MCP resource alone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<String>,
}

pub(crate) struct Store {
    path: PathBuf,
}

impl Store {
    pub fn open(options: &AuthOptions<LinkClient>, issuer: &str) -> Result<(Self, Record), AuthError> {
        fs::create_dir_all(&options.storage_dir).map_err(|_| AuthError::Storage)?;
        restrict_directory(&options.storage_dir)?;
        let store = Self {
            path: options.storage_dir.join("grant.json"),
        };
        match fs::read(&store.path) {
            Ok(bytes) => {
                let record: Record = serde_json::from_slice(&bytes).map_err(|_| AuthError::Storage)?;
                validate(&record, options, issuer)?;
                Ok((store, record))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let record = Record {
                    version: 1,
                    issuer: issuer.into(),
                    resource: options.resource_url.clone(),
                    client_id: match &options.client_id {
                        LinkClient::Explicit(id) => Some(id.clone()),
                        LinkClient::Automatic { .. } => None,
                    },
                    registration: None,
                    private_key_jwk: Arc::new(proof::generate()),
                    grant: None,
                };
                if record.client_id.is_some() {
                    store.save(&record)?;
                }
                Ok((store, record))
            }
            Err(_) => Err(AuthError::Storage),
        }
    }

    pub fn save(&self, record: &Record) -> Result<(), AuthError> {
        #[derive(Serialize)]
        struct Stored<'a> {
            version: u8,
            issuer: &'a str,
            resource: &'a str,
            client_id: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            registration: Option<&'a Registration>,
            private_key_jwk: &'a PrivateJwk,
            grant: Option<&'a Grant>,
        }
        let value = Stored {
            version: record.version,
            issuer: &record.issuer,
            resource: &record.resource,
            client_id: record.client_id.as_deref().ok_or(AuthError::Storage)?,
            registration: record.registration.as_ref(),
            private_key_jwk: record.private_key_jwk.expose(),
            grant: record.grant.as_ref().map(Secret::expose),
        };
        let bytes = Secret::new(serde_json::to_vec(&value).map_err(|_| AuthError::Storage)?);
        atomic_write(&self.path, bytes.expose())
    }
}

fn validate(record: &Record, options: &AuthOptions<LinkClient>, issuer: &str) -> Result<(), AuthError> {
    if record.version != 1 {
        return Err(AuthError::Storage);
    }
    proof::signing_key(record.private_key_jwk.expose())?;
    let client_id = record
        .client_id
        .as_deref()
        .filter(|id| crate::valid_text(id))
        .ok_or(AuthError::Storage)?;
    if record.issuer != issuer
        || record.resource != options.resource_url
        || matches!(&options.client_id, LinkClient::Explicit(id) if id != client_id)
    {
        return Err(AuthError::RelinkRequired);
    }
    if record
        .grant
        .as_ref()
        .is_some_and(|g| g.expose().access_token.is_empty() || g.expose().refresh_token.is_empty())
    {
        return Err(AuthError::Storage);
    }
    Ok(())
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), AuthError> {
    let parent = path.parent().ok_or(AuthError::Storage)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|_| AuthError::Storage)?;
    // NamedTempFile starts with mode 0600. Replacement never opens/truncates the old file.
    temp.write_all(bytes).map_err(|_| AuthError::Storage)?;
    temp.as_file().sync_all().map_err(|_| AuthError::Storage)?;
    temp.persist(path).map_err(|_| AuthError::Storage)?;
    #[cfg(unix)]
    fs::File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|_| AuthError::Storage)?;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn restrict_directory(path: &Path) -> Result<(), AuthError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| AuthError::Storage)
}
