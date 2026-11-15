use crate::{
    AuthError, AuthOptions,
    proof::{self, PrivateJwk},
};
use echo_contracts::secret::Secret;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub version: u8,
    pub issuer: String,
    pub resource: String,
    pub client_id: String,
    pub private_key_jwk: Secret<PrivateJwk>,
    pub grant: Option<Secret<Grant>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Grant {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_unix: u64,
    pub scope: String,
}

pub(crate) struct Store {
    path: PathBuf,
}

impl Store {
    pub fn open(options: &AuthOptions, issuer: &str) -> Result<(Self, Record), AuthError> {
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
                    client_id: options.client_id.clone(),
                    private_key_jwk: proof::generate(),
                    grant: None,
                };
                store.save(&record)?;
                Ok((store, record))
            }
            Err(_) => Err(AuthError::Storage),
        }
    }

    pub fn save(&self, record: &Record) -> Result<(), AuthError> {
        let bytes = Secret::new(
            serde_json::to_vec(&json!({
                "version":record.version, "issuer":record.issuer, "resource":record.resource,
                "client_id":record.client_id, "private_key_jwk":record.private_key_jwk.expose(),
                "grant":record.grant.as_ref().map(Secret::expose),
            }))
            .map_err(|_| AuthError::Storage)?,
        );
        atomic_write(&self.path, bytes.expose())
    }
}

fn validate(record: &Record, options: &AuthOptions, issuer: &str) -> Result<(), AuthError> {
    if record.version != 1 {
        return Err(AuthError::Storage);
    }
    proof::signing_key(record.private_key_jwk.expose())?;
    if record.issuer != issuer || record.resource != options.resource_url || record.client_id != options.client_id {
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

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), AuthError> {
    let parent = path.parent().ok_or(AuthError::Storage)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|_| AuthError::Storage)?;
    // NamedTempFile starts with mode 0600 on Unix; Windows inherits the restricted
    // directory ACL. Replacement never opens/truncates the old file.
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
fn restrict_directory(path: &Path) -> Result<(), AuthError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| AuthError::Storage)
}

#[cfg(windows)]
fn restrict_directory(path: &Path) -> Result<(), AuthError> {
    use std::os::windows::process::CommandExt;
    // Use the OS ACL API through its installed shell to keep this safe Rust.
    // The path travels as a child-only environment value, never as shell code;
    // neither the identity nor command output is included in diagnostics.
    let script = r"$ErrorActionPreference = 'Stop'
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
$acl = [System.Security.AccessControl.DirectorySecurity]::new()
$acl.SetOwner($sid)
$acl.SetAccessRuleProtection($true, $false)
$rule = [System.Security.AccessControl.FileSystemAccessRule]::new($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
$acl.AddAccessRule($rule)
[System.IO.Directory]::SetAccessControl($env:ECHO_AUTH_STORAGE_DIR, $acl)";
    let result = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("ECHO_AUTH_STORAGE_DIR", path)
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|_| AuthError::Storage)?;
    if !result.status.success() {
        return Err(AuthError::Storage);
    }
    Ok(())
}
