use crate::{
    AuthError, proof,
    store::{Grant, Record},
};
use echo_contracts::secret::Secret;
use reqwest::Client;
use serde::Deserialize;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Deserialize)]
pub(crate) struct Pair {
    pub access_token: Secret<String>,
    pub refresh_token: Secret<String>,
    pub token_type: String,
    pub expires_in: u64,
    pub scope: String,
}

impl Pair {
    pub fn grant(self, now: u64) -> Result<Secret<Grant>, AuthError> {
        if !self.token_type.eq_ignore_ascii_case("Bearer")
            || self.access_token.expose().is_empty()
            || self.refresh_token.expose().is_empty()
            || self.expires_in == 0
        {
            return Err(AuthError::Protocol);
        }
        Ok(Secret::new(Grant {
            access_token: self.access_token.expose().clone(),
            refresh_token: self.refresh_token.expose().clone(),
            expires_at_unix: now.checked_add(self.expires_in).ok_or(AuthError::Protocol)?,
            scope: self.scope,
        }))
    }
}

#[derive(Deserialize)]
struct Failure {
    error: String,
}

pub(crate) enum Reply<T> {
    Value(T),
    Pending,
    SlowDown,
}

pub(crate) async fn post<T: serde::de::DeserializeOwned>(
    client: &Client,
    endpoint: &str,
    form: &[(&str, &str)],
    key: Option<&Record>,
    now: u64,
) -> Result<Reply<T>, AuthError> {
    let mut request = client.post(endpoint).form(form);
    if let Some(record) = key {
        let proof = proof::sign(record.private_key_jwk.expose(), endpoint, now)?;
        request = request.header("DPoP", proof.expose());
    }
    let reply = request.send().await.map_err(|_| AuthError::Transport)?;
    if reply.status().is_success() {
        return reply.json().await.map(Reply::Value).map_err(|_| AuthError::Protocol);
    }
    if reply.status().as_u16() != 400 {
        return Err(AuthError::Transport);
    }
    let failure: Failure = reply.json().await.map_err(|_| AuthError::Protocol)?;
    match failure.error.as_str() {
        "authorization_pending" => Ok(Reply::Pending),
        "slow_down" => Ok(Reply::SlowDown),
        "access_denied" => Err(AuthError::AccessDenied),
        "expired_token" => Err(AuthError::Expired),
        "invalid_grant" => Err(AuthError::RelinkRequired),
        _ => Err(AuthError::Protocol),
    }
}

pub(crate) fn unix_now() -> Result<u64, AuthError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| AuthError::Protocol)
}
