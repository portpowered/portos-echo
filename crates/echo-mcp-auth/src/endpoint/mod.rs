//! Install/rename uses the endpoint audience on a redirect-free HTTPS client.

use crate::{TokenFuture, discovery, valid_text};
use echo_contracts::endpoint::{InstallRequest, InstallResult, RoutePage};
use reqwest::{Client, StatusCode, redirect::Policy};
use std::time::Duration;
use url::Url;

mod registration;

pub use registration::{EndpointInstall, EndpointInstallExchange, EndpointRegistration, new_request_id};

const EXCHANGE_PATH: &str = "/plugin-auth-code-exchanges";
const MAX_EXCHANGE_RESPONSE: usize = 16_384;
const ROUTES_PATH: &str = "/routes";
const MAX_ROUTES_RESPONSE: usize = 1024 * 1024;
/// Route pages read before giving up on one discovery attempt.
const MAX_ROUTE_PAGES: usize = 100;

/// Provides only an endpoint-audience bearer for service REST and gateway.
pub trait EndpointTokenSource: Send + Sync {
    fn access_token(&self) -> TokenFuture<'_>;

    /// A newly refreshed token, for a gateway that ended the connection's
    /// token lifetime (close 4000) or refused the current one.
    fn fresh_token(&self) -> TokenFuture<'_> {
        self.access_token()
    }
}

/// What one route read-back saw, as counts only: never ids.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RouteScan {
    /// Pages Port OS answered.
    pub pages: usize,
    /// Route records across those pages.
    pub results: usize,
    /// Records that carry an `associatedPluginRouteId` at all.
    pub routed: usize,
}

pub struct EndpointExchange {
    url: Url,
    routes: Url,
    client: Client,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ExchangeError {
    #[error("Endpoint API is unavailable")]
    Unavailable,
    #[error("Endpoint grant is unavailable")]
    Grant,
    /// No HTTP answer arrived. The caller retries.
    #[error("Endpoint exchange did not complete")]
    Transport(Unanswered),
    /// Port OS answered with this server error status. The caller retries.
    #[error("Port OS could not serve the endpoint exchange")]
    Server(u16),
    #[error("Port OS refused to create an already paired endpoint")]
    Conflict,
    /// Port OS answered with this other non-success status.
    #[error("Port OS refused the endpoint exchange")]
    Rejected(u16),
    #[error("Port OS returned an invalid endpoint exchange response")]
    Protocol,
}

/// Why no HTTP answer arrived, as a fixed category; never the error text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unanswered {
    Timeout,
    /// The TLS handshake failed, for example on an untrusted server certificate.
    Tls,
    Connect,
    /// The answer's body was interrupted.
    Body,
    Request,
}

impl ExchangeError {
    /// Whether the same committed request may be sent again unchanged.
    #[must_use]
    pub fn retryable(self) -> bool {
        matches!(self, Self::Transport(_) | Self::Server(_))
    }

    /// A fixed reason code for traces.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Grant => "grant",
            Self::Transport(cause) => cause.reason(),
            Self::Server(_) => "server_error",
            Self::Conflict => "conflict",
            Self::Rejected(_) => "rejected",
            Self::Protocol => "protocol",
        }
    }

    /// The HTTP status Port OS answered with, when it answered with a failure.
    #[must_use]
    pub fn http_status(self) -> Option<u16> {
        match self {
            Self::Server(status) | Self::Rejected(status) => Some(status),
            Self::Conflict => Some(StatusCode::CONFLICT.as_u16()),
            _ => None,
        }
    }
}

impl Unanswered {
    /// A fixed reason code for traces.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Tls => "tls",
            Self::Connect => "connect",
            Self::Body => "body",
            Self::Request => "request",
        }
    }

    /// Classify an HTTP client failure without keeping its text, which names the URL.
    #[must_use]
    pub fn of(error: &reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::Timeout
        } else if error.is_body() || error.is_decode() {
            Self::Body
        } else if tls_failure(error) {
            Self::Tls
        } else if error.is_connect() {
            Self::Connect
        } else {
            Self::Request
        }
    }
}

/// rustls reports a failed handshake as `InvalidData` I/O beneath the connect
/// error, possibly wrapped in further I/O errors, whose `source` skips the
/// wrapped error itself.
fn tls_failure(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut source = error.source();
    while let Some(cause) = source {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            if io.kind() == std::io::ErrorKind::InvalidData {
                return true;
            }
            source = io.get_ref().map(|inner| inner as &(dyn std::error::Error + 'static));
        } else {
            source = cause.source();
        }
    }
    false
}

fn unanswered(error: &reqwest::Error) -> ExchangeError {
    ExchangeError::Transport(Unanswered::of(error))
}

fn refused(status: StatusCode) -> Option<ExchangeError> {
    if status == StatusCode::CONFLICT {
        Some(ExchangeError::Conflict)
    } else if status.is_server_error() {
        Some(ExchangeError::Server(status.as_u16()))
    } else if status.is_success() {
        None
    } else {
        Some(ExchangeError::Rejected(status.as_u16()))
    }
}

impl EndpointExchange {
    /// Prepare the fixed HTTPS endpoint API path without contacting it. Native
    /// trust is kept; `ca_pem` adds the bridge's configured roots, the bundle
    /// the Port OS grant and the MCP client trust (`codex.ca_file`).
    ///
    /// # Errors
    /// Refuses unsafe URLs, malformed or empty roots and invalid HTTP client configuration.
    pub fn open(api_base: &str, timeout: Duration, ca_pem: Option<&[u8]>) -> Result<Self, ExchangeError> {
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
        Self::new(api, timeout, ca_pem)
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
        Self::new(url, timeout, None)
    }

    fn new(mut url: Url, timeout: Duration, ca_pem: Option<&[u8]>) -> Result<Self, ExchangeError> {
        if timeout.is_zero() {
            return Err(ExchangeError::Unavailable);
        }
        url.set_path(EXCHANGE_PATH);
        let mut routes = url.clone();
        routes.set_path(ROUTES_PATH);
        routes
            .query_pairs_mut()
            .append_pair("owner", "~caller")
            .append_pair("maxResults", "100");
        let mut builder = Client::builder()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .timeout(timeout);
        if let Some(pem) = ca_pem {
            let roots = reqwest::Certificate::from_pem_bundle(pem).map_err(|_| ExchangeError::Unavailable)?;
            if roots.is_empty() {
                return Err(ExchangeError::Unavailable);
            }
            for root in roots {
                builder = builder.add_root_certificate(root);
            }
        }
        let client = builder.build().map_err(|_| ExchangeError::Unavailable)?;
        Ok(Self { url, routes, client })
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
            .map_err(|error| unanswered(&error))?;
        if let Some(error) = refused(response.status()) {
            return Err(error);
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

impl EndpointExchange {
    /// Read back the caller's routes and return the endpoint id of the one
    /// whose plugin route id is this `deviceId`. `None` while discovery lags.
    /// Pages follow `nextToken` until it is absent or the route is found.
    /// The id is for display and logs; nothing is derived from it.
    ///
    /// # Errors
    /// Returns a safe diagnostic without including request or response data.
    pub async fn endpoint_id(
        &self,
        tokens: &dyn EndpointTokenSource,
        device_id: &str,
    ) -> Result<Option<String>, ExchangeError> {
        self.read_routes(tokens, device_id, &mut RouteScan::default()).await
    }

    /// As [`Self::endpoint_id`], counting what the read-back saw into `scan`
    /// so a failed or empty attempt can be traced without its contents.
    ///
    /// # Errors
    /// Returns a safe diagnostic without including request or response data.
    pub async fn read_routes(
        &self,
        tokens: &dyn EndpointTokenSource,
        device_id: &str,
        scan: &mut RouteScan,
    ) -> Result<Option<String>, ExchangeError> {
        let token = tokens.access_token().await.map_err(|_| ExchangeError::Grant)?;
        let mut next: Option<String> = None;
        for _ in 0..MAX_ROUTE_PAGES {
            let mut url = self.routes.clone();
            if let Some(next) = &next {
                url.query_pairs_mut().append_pair("nextToken", next);
            }
            let page = self.route_page(url, token.expose()).await?;
            scan.pages += 1;
            scan.results += page.results.len();
            scan.routed += page
                .results
                .iter()
                .filter(|route| route.associated_plugin_route_id.is_some())
                .count();
            if let Some(endpoint) = page.endpoint_for(device_id) {
                return Ok(Some(endpoint.to_owned()));
            }
            match page.next_token.filter(|token| !token.is_empty()) {
                None => return Ok(None),
                Some(token) if next.as_ref() == Some(&token) => return Err(ExchangeError::Protocol),
                Some(token) => next = Some(token),
            }
        }
        Err(ExchangeError::Protocol)
    }

    async fn route_page(&self, url: Url, token: &str) -> Result<RoutePage, ExchangeError> {
        let response = self
            .client
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|error| unanswered(&error))?;
        if let Some(error) = refused(response.status()) {
            return Err(error);
        }
        read_json(response, MAX_ROUTES_RESPONSE).await
    }
}

async fn read_result(response: reqwest::Response) -> Result<InstallResult, ExchangeError> {
    read_json(response, MAX_EXCHANGE_RESPONSE).await
}

async fn read_json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<T, ExchangeError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| unanswered(&error))? {
        if chunk.len() > limit - body.len() {
            return Err(ExchangeError::Protocol);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| ExchangeError::Protocol)
}

#[cfg(test)]
mod tests;
