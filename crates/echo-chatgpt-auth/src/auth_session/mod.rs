mod auth_import;
mod auth_lock;

pub(crate) use auth_import::import;
pub(crate) use auth_lock::{lock, mark, pending};

use crate::{AuthError, AuthSource, ChatGptCredential, auth_http, auth_store, auth_token};
use std::time::Duration;

pub(crate) async fn refresh(source: &AuthSource, old: auth_store::Record) -> Result<ChatGptCredential, AuthError> {
    let request = source.client.post(&source.config.token_endpoint).form(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", old.refresh.expose()),
        ("client_id", &source.config.client_id),
    ]);
    let (status, bytes) = auth_http::request(request, Duration::from_secs(30)).await?;
    if !status.is_success() {
        return Err(AuthError::RelinkRequired);
    }
    let record = auth_token::refresh_response(&bytes, source.clock.unix()?, old.refresh)?;
    if record.account_id != old.account_id {
        return Err(AuthError::AccountMismatch);
    }
    auth_store::save(&source.file, &record)?;
    Ok(record.credential())
}
