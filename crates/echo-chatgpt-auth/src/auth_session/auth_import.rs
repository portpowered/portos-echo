use crate::{AuthError, AuthSource, ChatGptCredential, auth_store, auth_token};
use echo_contracts::secret::Secret;
use serde::Deserialize;
use std::{fs::File, io::Read, path::Path};

#[derive(Deserialize)]
struct Imported {
    tokens: Tokens,
    last_refresh: Option<String>,
}
#[derive(Deserialize)]
struct Tokens {
    access_token: Secret<String>,
    refresh_token: Secret<String>,
    account_id: String,
}
pub(crate) fn import(source: &AuthSource, path: &Path) -> Result<ChatGptCredential, AuthError> {
    if source.file.try_exists().map_err(|_| AuthError::Storage)? {
        return Err(AuthError::AlreadyImported);
    }
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
    let imported: Imported = serde_json::from_slice(&bytes).map_err(|_| AuthError::InvalidCredential)?;
    let (account_id, expires) = auth_token::claims(imported.tokens.access_token.expose())?;
    if account_id != imported.tokens.account_id {
        return Err(AuthError::AccountMismatch);
    }
    let last_refresh = imported
        .last_refresh
        .as_deref()
        .map(timestamp)
        .transpose()?
        .unwrap_or(0);
    let record = auth_store::Record {
        access: imported.tokens.access_token,
        refresh: imported.tokens.refresh_token,
        expires,
        account_id,
        last_refresh,
    };
    auth_store::save(&source.file, &record)?;
    Ok(record.credential())
}
fn timestamp(value: &str) -> Result<u64, AuthError> {
    let date = time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .map_err(|_| AuthError::InvalidCredential)?;
    u64::try_from(date.unix_timestamp()).map_err(|_| AuthError::InvalidCredential)
}
