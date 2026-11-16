use crate::{AuthError, auth_store::Record};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use echo_contracts::secret::Secret;
use serde::Deserialize;

#[derive(Deserialize)]
struct Claims {
    exp: u64,
    #[serde(rename = "https://api.openai.com/auth")]
    auth: Option<Account>,
}
#[derive(Deserialize)]
struct Account {
    chatgpt_account_id: Option<String>,
}

pub(crate) fn nonempty(value: &str) -> Result<(), AuthError> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        Err(AuthError::InvalidCredential)
    } else {
        Ok(())
    }
}
pub(crate) fn claims(token: &str) -> Result<(String, u64), AuthError> {
    if token.len() > 256 * 1024 {
        return Err(AuthError::InvalidCredential);
    }
    let mut parts = token.split('.');
    let header = parts.next().ok_or(AuthError::InvalidCredential)?;
    let payload = parts.next().ok_or(AuthError::InvalidCredential)?;
    let signature = parts.next().ok_or(AuthError::InvalidCredential)?;
    if header.is_empty() || signature.is_empty() || parts.next().is_some() {
        return Err(AuthError::InvalidCredential);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| AuthError::InvalidCredential)?;
    let claims: Claims = serde_json::from_slice(&bytes).map_err(|_| AuthError::InvalidCredential)?;
    let id = claims
        .auth
        .and_then(|a| a.chatgpt_account_id)
        .ok_or(AuthError::AccountIdMissing)?;
    nonempty(&id)?;
    if claims.exp == 0 {
        return Err(AuthError::InvalidCredential);
    }
    Ok((id, claims.exp))
}
#[derive(Deserialize)]
struct Response {
    access_token: Secret<String>,
    refresh_token: Secret<String>,
    expires_in: Option<u64>,
}
pub(crate) fn response(bytes: &[u8], now: u64) -> Result<Record, AuthError> {
    let response: Response = super::auth_http::decode(bytes)?;
    record(response, now)
}
pub(crate) fn refresh_response(bytes: &[u8], now: u64, old: Secret<String>) -> Result<Record, AuthError> {
    #[derive(Deserialize)]
    struct Rotation {
        access_token: Secret<String>,
        refresh_token: Option<Secret<String>>,
        expires_in: Option<u64>,
    }
    let value: serde_json::Value = super::auth_http::decode(bytes)?;
    if value.get("refresh_token").is_some_and(serde_json::Value::is_null) {
        return Err(AuthError::InvalidCredential);
    }
    let response: Rotation = serde_json::from_value(value).map_err(|_| AuthError::InvalidCredential)?;
    record(
        Response {
            access_token: response.access_token,
            refresh_token: response.refresh_token.unwrap_or(old),
            expires_in: response.expires_in,
        },
        now,
    )
}
fn record(response: Response, now: u64) -> Result<Record, AuthError> {
    nonempty(response.refresh_token.expose())?;
    let (account_id, jwt_expiry) = claims(response.access_token.expose())?;
    let expires = response.expires_in.map_or(Ok(jwt_expiry), |seconds| {
        now.checked_add(seconds)
            .filter(|t| *t > now)
            .ok_or(AuthError::InvalidCredential)
    })?;
    if expires <= now || jwt_expiry <= now {
        return Err(AuthError::InvalidCredential);
    }
    Ok(Record {
        access: response.access_token,
        refresh: response.refresh_token,
        expires: expires.min(jwt_expiry),
        account_id,
        last_refresh: now,
    })
}
