pub(crate) use crate::refusal::Refusal;
use crate::{AuthError, proof, store::Grant};
use echo_contracts::secret::Secret;
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

pub(crate) async fn register(client: &Client, endpoint: url::Url, name: &str, scope: &str) -> Result<String, Refusal> {
    #[derive(Deserialize)]
    struct RegistrationReply {
        client_id: String,
    }
    let response = client
        .post(endpoint)
        .json(&serde_json::json!({
            "client_name": name,
            "token_endpoint_auth_method": "none",
            "grant_types": [DEVICE_GRANT, "refresh_token"],
            "scope": scope,
        }))
        .send()
        .await
        .map_err(|_| Refusal::unsent(AuthError::Transport))?;
    let status = response.status();
    match status {
        StatusCode::CREATED => {
            let result: Option<RegistrationReply> = response.json().await.ok();
            result
                .map(|reply| reply.client_id)
                .filter(|id| crate::valid_text(id))
                .ok_or_else(|| Refusal::status(AuthError::Protocol, status))
        }
        StatusCode::BAD_REQUEST => {
            let Ok(failure) = response.json::<Failure>().await else {
                return Err(Refusal::status(AuthError::Protocol, status));
            };
            let error = if failure.error == "invalid_client_metadata" {
                AuthError::RegistrationRefused
            } else {
                AuthError::Protocol
            };
            Err(Refusal::coded(error, status, &failure.error))
        }
        _ => Err(Refusal::status(AuthError::Transport, status)),
    }
}

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
    key: Option<&proof::PrivateJwk>,
    now: u64,
) -> Result<Reply<T>, Refusal> {
    let mut request = client.post(endpoint).form(form);
    if let Some(key) = key {
        let proof = proof::sign(key, endpoint, now).map_err(Refusal::unsent)?;
        request = request.header("DPoP", proof.expose());
    }
    let reply = request
        .send()
        .await
        .map_err(|_| Refusal::unsent(AuthError::Transport))?;
    let status = reply.status();
    if status.is_success() {
        let value = reply.json().await.map(Reply::Value);
        return value.map_err(|_| Refusal::status(AuthError::Protocol, status));
    }
    if status != StatusCode::BAD_REQUEST {
        return Err(Refusal::status(AuthError::Transport, status));
    }
    let Ok(failure) = reply.json::<Failure>().await else {
        return Err(Refusal::status(AuthError::Protocol, status));
    };
    let error = match failure.error.as_str() {
        "authorization_pending" => return Ok(Reply::Pending),
        "slow_down" => return Ok(Reply::SlowDown),
        "access_denied" => AuthError::AccessDenied,
        "expired_token" => AuthError::Expired,
        "invalid_grant" => AuthError::RelinkRequired,
        _ => AuthError::Protocol,
    };
    Err(Refusal::coded(error, status, &failure.error))
}

pub(crate) fn unix_now() -> Result<u64, AuthError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| AuthError::Protocol)
}
