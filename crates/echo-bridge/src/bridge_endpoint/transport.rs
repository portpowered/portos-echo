use super::{Status, commands::Receipts, snapshot::Camera, voice::Voice};
use echo_contracts::endpoint::Uplink;
use echo_mcp_auth::{EndpointTokenSource, new_request_id};
use futures_util::{SinkExt, StreamExt, stream::FuturesUnordered};
use reqwest::{Client, Url};
use std::{fmt::Write as _, time::Duration};
use tokio::{
    net::TcpStream,
    sync::mpsc,
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
pub(super) const MAX_MESSAGE: usize = 8 * 1024 * 1024;
const QUEUED_COMMANDS: usize = 32;
const IN_FLIGHT_REPLIES: usize = 16;
const PATH_MARKER: &str = "{authLinkId}";

pub(super) struct Transport {
    gateway: String,
    messages: Url,
    http: Client,
    pub(super) camera: Camera,
    pub(super) trace: Option<crate::bridge_trace::Trace>,
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Frame {
    text: tokio_tungstenite::tungstenite::Utf8Bytes,
    received: Instant,
}

impl Transport {
    pub(super) fn new(gateway: &str, api: &str, loopback: bool) -> Option<Self> {
        let mut messages = Url::parse(api).ok()?;
        let gateway = gateway.strip_suffix(PATH_MARKER)?;
        let url = Url::parse(gateway).ok()?;
        let schemes = (messages.scheme(), url.scheme());
        let encrypted = schemes == ("https", "wss");
        let synthetic = loopback
            && schemes == ("http", "ws")
            && messages.host_str() == Some("127.0.0.1")
            && url.host_str() == Some("127.0.0.1");
        if !(encrypted || synthetic) {
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
            camera: Camera::default(),
            trace: None,
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
        let (intake, frames) = mpsc::channel(QUEUED_COMMANDS);
        tokio::select! {
            status = self.gateway(auth_link, route, tokens, state, &intake) => status,
            () = self.respond(frames, route, tokens) => Status::Unavailable,
        }
    }

    async fn gateway(
        &self,
        auth_link: &str,
        route: &str,
        tokens: &dyn EndpointTokenSource,
        state: &tokio::sync::watch::Sender<Status>,
        intake: &mpsc::Sender<Frame>,
    ) -> Status {
        let mut delay = Duration::from_secs(1);
        loop {
            match self.handshake(auth_link, tokens).await {
                Ok(socket) => {
                    state.send_replace(Status::Connected);
                    delay = Duration::from_secs(1);
                    self.connected(socket, route, tokens, intake).await;
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

    async fn connected(
        &self,
        socket: Socket,
        route: &str,
        tokens: &dyn EndpointTokenSource,
        intake: &mpsc::Sender<Frame>,
    ) {
        tokio::select! {
            () = Self::receive(socket, intake) => {}
            () = self.report(route, tokens) => {}
        }
    }

    async fn report(&self, route: &str, tokens: &dyn EndpointTokenSource) {
        let voice = Voice::current().unwrap_or_default();
        let mut states = voice.subscribe();
        let mut sent = None;
        loop {
            let body = {
                let view = states.borrow_and_update();
                serde_json::to_vec(&Uplink::new("report-state", route, view.report())).ok()
            };
            if body != sent {
                sent.clone_from(&body);
                self.send(tokens, body, Instant::now() + REPLY_WINDOW).await;
            }
            if states.changed().await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn receive(mut socket: Socket, intake: &mpsc::Sender<Frame>) {
        while let Some(Ok(message)) = socket.next().await {
            match message {
                Message::Close(_) => {
                    let _ = socket.flush().await;
                    break;
                }
                Message::Ping(_) if socket.flush().await.is_err() => break,
                Message::Text(text) => {
                    let _ = intake.try_send(Frame {
                        text,
                        received: Instant::now(),
                    });
                }
                _ => {}
            }
        }
    }

    async fn respond(&self, mut frames: mpsc::Receiver<Frame>, route: &str, tokens: &dyn EndpointTokenSource) {
        let (_generation, lease) = tokio::sync::watch::channel(());
        let voice = Voice::current().unwrap_or_default();
        let mut terminal = voice.terminal();
        let mut receipts = Receipts::default();
        let mut replies = FuturesUnordered::new();
        loop {
            tokio::select! {
                _ = replies.next(), if !replies.is_empty() => {}
                ended = terminal.recv(), if replies.len() < IN_FLIGHT_REPLIES => {
                    if let Ok(ended) = ended {
                        replies.push(Box::pin(async move {
                            let body = ended.time().ok().and_then(|at| {
                                serde_json::to_vec(&Uplink::new("session-ended", route, ended.payload(&at))).ok()
                            });
                            self.send(tokens, body, ended.instant + REPLY_WINDOW).await;
                        }) as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>);
                    }
                }
                frame = frames.recv(), if replies.len() < IN_FLIGHT_REPLIES => {
                    let Some(frame) = frame else { return };
                    let deadline = frame.received + REPLY_WINDOW;
                    if let Some(command) = receipts.accept(route, &frame.text, frame.received, deadline) {
                        let lease = lease.clone();
                        replies.push(Box::pin(async move {
                            let body = command.reply(route, lease, &self.camera, self.trace.as_ref()).await;
                            self.send(tokens, body, deadline).await;
                        }));
                    }
                }
            }
        }
    }

    async fn send(&self, tokens: &dyn EndpointTokenSource, body: Option<Vec<u8>>, deadline: Instant) {
        let Some(body) = body.filter(|bytes| bytes.len() <= MAX_MESSAGE) else {
            return;
        };
        let Ok(key) = new_request_id() else { return };
        while Instant::now() < deadline {
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
