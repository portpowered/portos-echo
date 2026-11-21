//! Install/rename uses the endpoint audience on a redirect-free HTTPS client.

use crate::{TokenFuture, discovery, valid_text};
use echo_contracts::endpoint::{InstallRequest, InstallResult};
use reqwest::{Client, StatusCode, redirect::Policy};
use std::time::Duration;
use url::Url;

mod grant;
mod registration;

pub use grant::{EndpointApproval, EndpointGrant};
pub use registration::{EndpointInstall, EndpointInstallExchange, EndpointRegistration, new_request_id};

const EXCHANGE_PATH: &str = "/plugin-auth-code-exchanges";
const MAX_EXCHANGE_RESPONSE: usize = 16_384;

/// Provides only an endpoint-audience bearer for service REST and gateway.
pub trait EndpointTokenSource: Send + Sync {
    fn access_token(&self) -> TokenFuture<'_>;
}

pub struct EndpointExchange {
    url: Url,
    client: Client,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ExchangeError {
    #[error("Endpoint API is unavailable")]
    Unavailable,
    #[error("Endpoint grant is unavailable")]
    Grant,
    #[error("Endpoint exchange did not complete")]
    Transport,
    #[error("Port OS refused to create an already paired endpoint")]
    Conflict,
    #[error("Port OS refused the endpoint exchange")]
    Rejected,
    #[error("Port OS returned an invalid endpoint exchange response")]
    Protocol,
}

impl EndpointExchange {
    /// Prepare the fixed HTTPS endpoint API path without contacting it.
    ///
    /// # Errors
    /// Refuses unsafe URLs and invalid HTTP client configuration.
    pub fn open(api_base: &str, timeout: Duration) -> Result<Self, ExchangeError> {
        let authority = api_base
            .strip_prefix("https://")
            .and_then(|rest| rest.split('/').next())
            .ok_or(ExchangeError::Unavailable)?;
        if authority.contains('@')
            || api_base.contains(['?', '#', '{', '}', '\\'])
            || api_base.chars().any(|ch| ch.is_whitespace() || ch.is_control())
        {
            return Err(ExchangeError::Unavailable);
        }
        let api = discovery::safe_url(api_base).map_err(|_| ExchangeError::Unavailable)?;
        if api.scheme() != "https" || authority.is_empty() {
            return Err(ExchangeError::Unavailable);
        }
        Self::new(api, timeout)
    }

    /// Construct the exchange adapter against a synthetic loopback issuer.
    ///
    /// # Errors
    /// Refuses any address other than literal IPv4 loopback.
    #[cfg(feature = "mock")]
    pub fn loopback(api_base: &str, timeout: Duration) -> Result<Self, ExchangeError> {
        let url = discovery::safe_url(api_base).map_err(|_| ExchangeError::Unavailable)?;
        if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") {
            return Err(ExchangeError::Unavailable);
        }
        Self::new(url, timeout)
    }

    fn new(mut url: Url, timeout: Duration) -> Result<Self, ExchangeError> {
        if timeout.is_zero() {
            return Err(ExchangeError::Unavailable);
        }
        url.set_path(EXCHANGE_PATH);
        let client = Client::builder()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .timeout(timeout)
            .build()
            .map_err(|_| ExchangeError::Unavailable)?;
        Ok(Self { url, client })
    }

    /// Send the committed request with its durable idempotency key.
    ///
    /// # Errors
    /// Returns a safe diagnostic without including request or response data.
    pub async fn submit(
        &self,
        tokens: &dyn EndpointTokenSource,
        idempotency_key: &str,
        request: &InstallRequest<'_>,
    ) -> Result<InstallResult, ExchangeError> {
        let token = tokens.access_token().await.map_err(|_| ExchangeError::Grant)?;
        let response = self
            .client
            .post(self.url.clone())
            .bearer_auth(token.expose())
            .header("Idempotency-Key", idempotency_key)
            .json(request)
            .send()
            .await
            .map_err(|_| ExchangeError::Transport)?;
        let status = response.status();
        if status == StatusCode::CONFLICT {
            return Err(ExchangeError::Conflict);
        }
        if status.is_server_error() {
            return Err(ExchangeError::Transport);
        }
        if !status.is_success() {
            return Err(ExchangeError::Rejected);
        }
        let result = read_result(response).await?;
        if request.auth_link_id().is_some_and(|link| link != result.auth_link_id)
            || [&result.auth_link_id, &result.id, &result.etag]
                .into_iter()
                .any(|value| !valid_text(value))
        {
            return Err(ExchangeError::Protocol);
        }
        Ok(result)
    }
}

async fn read_result(mut response: reqwest::Response) -> Result<InstallResult, ExchangeError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ExchangeError::Transport)? {
        if chunk.len() > MAX_EXCHANGE_RESPONSE - body.len() {
            return Err(ExchangeError::Protocol);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| ExchangeError::Protocol)
}

#[cfg(test)]
mod tests;
