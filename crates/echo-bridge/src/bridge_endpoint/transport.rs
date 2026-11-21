use super::Status;
use echo_contracts::endpoint::{AssistantState, ReportState, Uplink};
use echo_mcp_auth::{EndpointTokenSource, new_request_id};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, Url};
use std::{fmt::Write as _, time::Duration};
use tokio::{
    net::TcpStream,
    time::{Instant, sleep_until, timeout, timeout_at},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{HeaderValue, header::AUTHORIZATION},
        protocol::WebSocketConfig,
    },
};

pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const REPLY_WINDOW: Duration = Duration::from_secs(10);
const MAX_COMMAND: usize = 64 * 1024;
const MAX_MESSAGE: usize = 8 * 1024 * 1024;
const PATH_MARKER: &str = "{authLinkId}";

pub(super) struct Transport {
    gateway: String,
    messages: Url,
    http: Client,
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

impl Transport {
    pub(super) fn new(gateway: &str, api: &str, loopback: bool) -> Option<Self> {
        let mut messages = Url::parse(api).ok()?;
        let gateway = gateway.strip_suffix(PATH_MARKER)?;
        let url = Url::parse(gateway).ok()?;
        if !matches!((messages.scheme(), url.scheme()), ("https", "wss"))
            && !(loopback
                && messages.scheme() == "http"
                && url.scheme() == "ws"
                && messages.host_str() == Some("127.0.0.1")
                && url.host_str() == Some("127.0.0.1"))
        {
            return None;
        }
        if [&url, &messages].into_iter().any(|url| {
            url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
        }) {
            return None;
        }
        messages.set_path("/messages");
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .ok()?;
        Some(Self {
            gateway: gateway.into(),
            messages,
            http,
        })
    }

    async fn handshake(&self, auth_link: &str, tokens: &dyn EndpointTokenSource) -> Result<Socket, Status> {
        let url = addressed(&self.gateway, auth_link);
        let mut request = url.into_client_request().map_err(|_| Status::Unavailable)?;
        let token = tokens
            .access_token()
            .await
            .map_err(|error| super::auth_status(&error))?;
        let mut bearer =
            HeaderValue::from_str(&format!("Bearer {}", token.expose())).map_err(|_| Status::Unavailable)?;
        bearer.set_sensitive(true);
        request.headers_mut().insert(AUTHORIZATION, bearer);
        let config = WebSocketConfig::default()
            .max_frame_size(Some(MAX_COMMAND))
            .max_message_size(Some(MAX_COMMAND));
        match timeout(REQUEST_TIMEOUT, connect_async_with_config(request, Some(config), false)).await {
            Ok(Ok((socket, _))) => Ok(socket),
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response)))
                if response.status().is_client_error() || response.status().is_redirection() =>
            {
                Err(Status::Unavailable)
            }
            _ => Err(Status::Connecting),
        }
    }

    pub(super) async fn serve(
        &self,
        auth_link: &str,
        route: &str,
        tokens: &dyn EndpointTokenSource,
        state: &tokio::sync::watch::Sender<Status>,
    ) -> Status {
        let mut delay = Duration::from_secs(1);
        loop {
            match self.handshake(auth_link, tokens).await {
                Ok(socket) => {
                    state.send_replace(Status::Connected);
                    delay = Duration::from_secs(1);
                    self.connected(socket, route, tokens).await;
                }
                Err(Status::Connecting) => {}
                Err(terminal) => return terminal,
            }
            state.send_replace(Status::Connecting);
            let source = new_request_id().unwrap_or_default();
            let jitter = source
                .bytes()
                .fold(0_u64, |hash, byte| hash.wrapping_mul(31).wrapping_add(u64::from(byte)))
                % 251;
            let pause = delay
                .saturating_add(Duration::from_millis(jitter))
                .min(Duration::from_secs(60));
            tokio::time::sleep(pause).await;
            delay = delay.saturating_mul(2).min(Duration::from_secs(60));
        }
    }

    async fn connected(&self, socket: Socket, route: &str, tokens: &dyn EndpointTokenSource) {
        let report = Uplink::new(
            "report-state",
            route,
            ReportState {
                state: AssistantState::Idle,
                session_id: None,
                origin: None,
                volume: None,
                muted: None,
            },
        );
        let receive = Self::receive(socket);
        tokio::pin!(receive);
        tokio::select! {
            () = &mut receive => {}
            () = self.send(tokens, serde_json::to_vec(&report).ok()) => receive.await,
        }
    }

    async fn receive(mut socket: Socket) {
        while let Some(Ok(message)) = socket.next().await {
            match message {
                Message::Close(_) => {
                    let _ = socket.flush().await;
                    break;
                }
                Message::Ping(_) if socket.flush().await.is_err() => break,
                _ => {}
            }
        }
    }

    async fn send(&self, tokens: &dyn EndpointTokenSource, body: Option<Vec<u8>>) {
        let Some(body) = body.filter(|bytes| bytes.len() <= MAX_MESSAGE) else {
            return;
        };
        let Ok(key) = new_request_id() else { return };
        let deadline = Instant::now() + REPLY_WINDOW;
        loop {
            let attempt = async {
                let Ok(token) = tokens.access_token().await else {
                    return false;
                };
                self.http
                    .post(self.messages.clone())
                    .bearer_auth(token.expose())
                    .header("Idempotency-Key", &key)
                    .header("Content-Type", "application/json")
                    .body(body.clone())
                    .send()
                    .await
                    .map_or(true, |reply| reply.status().is_server_error())
            };
            if !timeout_at(deadline, attempt).await.unwrap_or(false) {
                return;
            }
            let retry = Instant::now() + Duration::from_millis(250);
            if retry >= deadline {
                return;
            }
            sleep_until(retry).await;
        }
    }
}

fn addressed(prefix: &str, identity: &str) -> String {
    let mut url = String::from(prefix);
    for byte in identity.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'~') {
            url.push(char::from(byte));
        } else {
            let _ = write!(url, "%{byte:02X}");
        }
    }
    url
}
