use std::{future::Future, sync::Arc, time::Duration};

use echo_contracts::secret::Secret;
use futures_util::{Sink, Stream};
use reqwest::{Client, Response, StatusCode, Url, header};
use rustls::ClientConfig;
use serde::Deserialize;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{Error, Message, client::IntoClientRequest, protocol::WebSocketConfig},
};

use crate::{
    ServiceError,
    pairing::{Pairing, Tokens, parse_url, refresh_value, valid_token},
};

const MAX_API_JSON: usize = 256 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) struct Lease {
    pub(super) vm_id: String,
    pub(super) token: Secret<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UpgradeError {
    LeaseRejected,
    Unavailable,
}

pub(super) enum RefreshError {
    Revoked,
    Unavailable,
}

pub(super) trait Connector: Send + Sync + 'static {
    type Socket: Sink<Message, Error = Error> + Stream<Item = Result<Message, Error>> + Send + Unpin;

    fn lookup(&self, pairing: &Pairing) -> impl Future<Output = Result<Lease, ServiceError>> + Send;
    fn upgrade(
        &self,
        pairing: &Pairing,
        lease: &Lease,
    ) -> impl Future<Output = Result<Self::Socket, UpgradeError>> + Send;
    fn refresh(&self, pairing: &Pairing) -> impl Future<Output = Result<Tokens, RefreshError>> + Send;
}

pub(super) struct Network {
    api: Client,
    tls: Arc<ClientConfig>,
}

impl Network {
    pub(super) fn new(tls: ClientConfig) -> Result<Self, ServiceError> {
        let user_agent = format!(
            "musegadget/{} (Linux {})",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::ARCH
        );
        let api = Client::builder()
            .use_preconfigured_tls(tls.clone())
            .user_agent(user_agent)
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| ServiceError::TrustUnavailable)?;
        Ok(Self {
            api,
            tls: Arc::new(tls),
        })
    }
}

impl Connector for Network {
    type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

    async fn lookup(&self, pairing: &Pairing) -> Result<Lease, ServiceError> {
        let url = api_endpoint(pairing, &["fetch_vms"])?;
        let response = self
            .api
            .get(url)
            .bearer_auth(pairing.access_token())
            .header("X-API-Version", "1.0.0")
            .send()
            .await
            .map_err(|_| ServiceError::PeerUnavailable)?;
        select_lease(read_json(response).await?).ok_or(ServiceError::PeerUnavailable)
    }

    async fn upgrade(&self, pairing: &Pairing, lease: &Lease) -> Result<Self::Socket, UpgradeError> {
        let url = vm_url(pairing.noise_url.clone(), &lease.vm_id);
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|_| UpgradeError::Unavailable)?;
        let mut authorization = header::HeaderValue::from_str(&format!("Bearer {}", lease.token.expose()))
            .map_err(|_| UpgradeError::Unavailable)?;
        authorization.set_sensitive(true);
        request.headers_mut().insert(header::AUTHORIZATION, authorization);
        let config = WebSocketConfig::default()
            .max_message_size(Some(crate::noise::MAX_MESSAGE))
            .max_frame_size(Some(crate::noise::MAX_MESSAGE))
            .write_buffer_size(0);
        let (socket, _) = connect_async_tls_with_config(
            request,
            Some(config),
            false,
            Some(tokio_tungstenite::Connector::Rustls(self.tls.clone())),
        )
        .await
        .map_err(upgrade_error)?;
        Ok(socket)
    }

    async fn refresh(&self, pairing: &Pairing) -> Result<Tokens, RefreshError> {
        let url = api_endpoint(pairing, &["device_token", "refresh"]).map_err(|_| RefreshError::Unavailable)?;
        let token = refresh_value(pairing.refresh_token()).ok_or(RefreshError::Unavailable)?;
        let response = self
            .api
            .post(url)
            .bearer_auth(format!("hatch_refresh:{token}"))
            .json(&serde_json::json!({"device_id": pairing.node_id().expose()}))
            .send()
            .await
            .map_err(|_| RefreshError::Unavailable)?;
        if response.status() == StatusCode::UNAUTHORIZED {
            return Err(RefreshError::Revoked);
        }
        let body: RefreshResponse = read_json(response).await.map_err(|_| RefreshError::Unavailable)?;
        let tokens = match body {
            RefreshResponse::Wrapped { payload } => payload,
            RefreshResponse::Direct(tokens) => tokens,
        };
        tokens.valid().then_some(tokens).ok_or(RefreshError::Unavailable)
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RefreshResponse {
    Wrapped { payload: Tokens },
    Direct(Tokens),
}

fn api_endpoint(pairing: &Pairing, path: &[&str]) -> Result<Url, ServiceError> {
    let mut url = pairing.api_url.clone();
    url.path_segments_mut()
        .map_err(|()| ServiceError::UnsafePairing)?
        .pop_if_empty()
        .extend(path.iter().copied());
    Ok(url)
}

async fn read_json<T: for<'de> Deserialize<'de>>(mut response: Response) -> Result<T, ServiceError> {
    if !response.status().is_success() {
        return Err(ServiceError::PeerUnavailable);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ServiceError::PeerUnavailable)? {
        if chunk.len() > MAX_API_JSON - body.len() {
            return Err(ServiceError::PeerUnavailable);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| ServiceError::PeerUnavailable)
}

fn upgrade_error(error: Error) -> UpgradeError {
    match error {
        Error::Http(response) if matches!(response.status(), StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) => {
            UpgradeError::LeaseRejected
        }
        _ => UpgradeError::Unavailable,
    }
}

fn vm_url(mut url: Url, id: &str) -> Url {
    url.query_pairs_mut().append_pair("vm_id", id);
    if let Some(query) = url.query() {
        let encoded = query.replace('+', "%20");
        url.set_query(Some(&encoded));
    }
    url
}

#[derive(Deserialize)]
struct Leases {
    vm_list: Vec<Value>,
}

#[derive(Deserialize)]
struct Entry {
    vm_ws_url: Option<String>,
    vm_url: Option<String>,
    vm_auth_token: Secret<String>,
    vm_id: Option<String>,
    vm_name: Option<String>,
    #[serde(default)]
    default: bool,
}

impl Entry {
    fn usable(self) -> Option<(bool, Lease)> {
        self.vm_ws_url
            .iter()
            .chain(&self.vm_url)
            .find(|value| parse_url(value, "wss").is_ok() || parse_url(value, "https").is_ok())?;
        if !valid_token(self.vm_auth_token.expose()) {
            return None;
        }
        let vm_id = self
            .vm_id
            .into_iter()
            .chain(self.vm_name)
            .find(|id| !id.trim().is_empty())?;
        Some((
            self.default,
            Lease {
                vm_id,
                token: self.vm_auth_token,
            },
        ))
    }
}

fn select_lease(document: Leases) -> Option<Lease> {
    let mut first = None;
    for entry in document.vm_list {
        let Some((default, lease)) = serde_json::from_value::<Entry>(entry).ok().and_then(Entry::usable) else {
            continue;
        };
        if default {
            return Some(lease);
        }
        first.get_or_insert(lease);
    }
    first
}
