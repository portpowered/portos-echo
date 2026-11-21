mod uplink;

use super::{Status, commands::Receipts, snapshot::Camera, voice::Voice};
use echo_contracts::endpoint::{Report, Uplink};
use echo_mcp_auth::{EndpointTokenSource, new_request_id};
use futures_util::{SinkExt, StreamExt, stream::FuturesUnordered};
use reqwest::{Client, Url};
use std::{
    fmt::Write as _,
    time::{Duration, UNIX_EPOCH},
};
use tokio::{
    net::TcpStream,
    sync::mpsc,
    time::{Instant, MissedTickBehavior, interval_at, timeout},
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
const QUEUED_COMMANDS: usize = 32;
const IN_FLIGHT_REPLIES: usize = 16;
const PATH_MARKER: &str = "{authLinkId}";
/// Gateway close codes (live gateway README).
const CLOSE_CYCLE: u16 = 4000;
const CLOSE_UNAUTHORIZED: u16 = 4401;
const CLOSE_FORBIDDEN: u16 = 4403;
const CLOSE_REPLACED: u16 = 4409;
const CLOSE_RETRY: u16 = 4503;
/// Consecutive 4401 closes, each after a fresh token, before giving up.
const REFUSALS: u32 = 3;
/// A 4403 stops once a refreshed retry was already refused.
const FORBIDDEN_REFUSALS: u32 = 2;
/// The device sends the text frame `ping` this often; the gateway answers `pong`.
pub(super) const KEEPALIVE: Duration = Duration::from_secs(45);
const PING: &str = "ping";
const PONG: &str = "pong";

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

    async fn handshake(
        &self,
        auth_link: &str,
        tokens: &dyn EndpointTokenSource,
        fresh: bool,
    ) -> Result<Socket, Status> {
        let url = addressed(&self.gateway, auth_link);
        let mut request = url.into_client_request().map_err(|_| Status::Unavailable)?;
        let token = if fresh {
            tokens.fresh_token().await
        } else {
            tokens.access_token().await
        }
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

    /// Hold the gateway connection for `auth_link`; commands address `device_id`.
    pub(super) async fn serve(
        &self,
        auth_link: &str,
        device_id: &str,
        tokens: &dyn EndpointTokenSource,
        state: &tokio::sync::watch::Sender<Status>,
    ) -> Status {
        let (intake, frames) = mpsc::channel(QUEUED_COMMANDS);
        tokio::select! {
            status = self.gateway(auth_link, device_id, tokens, state, &intake) => status,
            () = self.respond(frames, device_id, tokens) => Status::Unavailable,
        }
    }

    async fn gateway(
        &self,
        auth_link: &str,
        device_id: &str,
        tokens: &dyn EndpointTokenSource,
        state: &tokio::sync::watch::Sender<Status>,
        intake: &mpsc::Sender<Frame>,
    ) -> Status {
        let mut retry = Retry::default();
        loop {
            match self.handshake(auth_link, tokens, retry.fresh).await {
                Ok(socket) => {
                    state.send_replace(Status::Connected);
                    let close = self.connected(socket, device_id, tokens, intake).await;
                    if let Some(status) = retry.closed(close) {
                        return status;
                    }
                    if close == Some(CLOSE_CYCLE) {
                        continue;
                    }
                }
                Err(Status::Connecting) => retry.fresh = false,
                Err(terminal) => return terminal,
            }
            state.send_replace(Status::Connecting);
            tokio::time::sleep(retry.pause()).await;
        }
    }

    async fn connected(
        &self,
        socket: Socket,
        device_id: &str,
        tokens: &dyn EndpointTokenSource,
        intake: &mpsc::Sender<Frame>,
    ) -> Option<u16> {
        tokio::select! {
            close = Self::receive(socket, intake) => close,
            () = self.report(device_id, tokens) => None,
        }
    }

    /// Report state whenever it changes; each report is stamped when it is sent.
    async fn report(&self, device_id: &str, tokens: &dyn EndpointTokenSource) {
        let voice = Voice::current().unwrap_or_default();
        let mut states = voice.subscribe();
        let mut sent: Option<Report> = None;
        loop {
            let report = states.borrow_and_update().report();
            if sent.as_ref() != Some(&report) {
                let mut stamped = report.clone();
                stamped.sampled_at_ms = wall_ms();
                sent = Some(report);
                let body = serde_json::to_vec(&Uplink::report(device_id, stamped)).ok();
                self.send(tokens, body, Instant::now() + REPLY_WINDOW).await;
            }
            if states.changed().await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Queue command frames and send the keepalive until the socket ends;
    /// returns the gateway close code.
    async fn receive(socket: Socket, intake: &mpsc::Sender<Frame>) -> Option<u16> {
        let (mut sink, mut stream) = socket.split();
        let mut keepalive = interval_at(Instant::now() + KEEPALIVE, KEEPALIVE);
        keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            let inbound = tokio::select! {
                message = stream.next() => inbound(message, intake),
                _ = keepalive.tick() => Inbound::Ping,
            };
            let sent = match inbound {
                Inbound::Closed(close) => {
                    let _ = sink.flush().await;
                    return close;
                }
                Inbound::Ping => sink.send(Message::text(PING)).await,
                Inbound::Flush => sink.flush().await,
                Inbound::Next => Ok(()),
            };
            if sent.is_err() {
                return None;
            }
        }
    }

    async fn respond(&self, mut frames: mpsc::Receiver<Frame>, device_id: &str, tokens: &dyn EndpointTokenSource) {
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
                                serde_json::to_vec(&Uplink::report(device_id, ended.report(at))).ok()
                            });
                            self.send(tokens, body, ended.instant + REPLY_WINDOW).await;
                        }) as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>);
                    }
                }
                frame = frames.recv(), if replies.len() < IN_FLIGHT_REPLIES => {
                    let Some(frame) = frame else { return };
                    let deadline = frame.received + REPLY_WINDOW;
                    if let Some(command) = receipts.accept(device_id, &frame.text, frame.received, deadline) {
                        let lease = lease.clone();
                        replies.push(Box::pin(async move {
                            let prepare = |report| serde_json::to_vec(&Uplink::report(device_id, report)).ok();
                            let Some(body) = command.reply(lease, &self.camera, self.trace.as_ref(), prepare).await else {
                                return;
                            };
                            self.send_reply(tokens, body, deadline).await;
                        }));
                    }
                }
            }
        }
    }
}

/// What one gateway message asks of the socket.
enum Inbound {
    Next,
    /// A protocol ping was read: flush the queued pong.
    Flush,
    Ping,
    Closed(Option<u16>),
}

/// Queue a command frame; the gateway's `pong` is never one.
fn inbound(
    message: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
    intake: &mpsc::Sender<Frame>,
) -> Inbound {
    match message {
        Some(Ok(Message::Close(frame))) => Inbound::Closed(frame.map(|frame| u16::from(frame.code))),
        Some(Ok(Message::Ping(_))) => Inbound::Flush,
        Some(Ok(Message::Text(text))) => {
            if !keepalive_reply(&text) {
                let _ = intake.try_send(Frame {
                    text,
                    received: Instant::now(),
                });
            }
            Inbound::Next
        }
        Some(Ok(_)) => Inbound::Next,
        Some(Err(_)) | None => Inbound::Closed(None),
    }
}

fn keepalive_reply(text: &str) -> bool {
    text == PONG
}

/// Reconnect policy across gateway closes.
struct Retry {
    delay: Duration,
    refusals: u32,
    fresh: bool,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            delay: Duration::from_secs(1),
            refusals: 0,
            fresh: false,
        }
    }
}

impl Retry {
    /// Account for one close. 4000 reconnects at once (the token refreshes
    /// only when near expiry); 4401 retries with a fresh token up to three
    /// times; 4403 stops after one refreshed retry; 4503 backs off and keeps
    /// the token; 4409 gives up.
    fn closed(&mut self, close: Option<u16>) -> Option<Status> {
        match close {
            Some(CLOSE_REPLACED) => return Some(Status::Unavailable),
            Some(code @ (CLOSE_UNAUTHORIZED | CLOSE_FORBIDDEN)) => {
                self.refusals += 1;
                let limit = if code == CLOSE_FORBIDDEN {
                    FORBIDDEN_REFUSALS
                } else {
                    REFUSALS
                };
                if self.refusals >= limit {
                    return Some(Status::Unavailable);
                }
                self.fresh = true;
                return None;
            }
            Some(CLOSE_RETRY) => {
                self.fresh = false;
                return None;
            }
            _ => self.fresh = false,
        }
        self.refusals = 0;
        self.delay = Duration::from_secs(1);
        None
    }

    fn pause(&mut self) -> Duration {
        let source = new_request_id().unwrap_or_default();
        let jitter = source
            .bytes()
            .fold(0_u64, |hash, byte| hash.wrapping_mul(31).wrapping_add(u64::from(byte)))
            % 251;
        let pause = self
            .delay
            .saturating_add(Duration::from_millis(jitter))
            .min(Duration::from_secs(60));
        self.delay = self.delay.saturating_mul(2).min(Duration::from_secs(60));
        pause
    }
}

/// Corrected wall-clock milliseconds since the Unix epoch.
fn wall_ms() -> Option<i64> {
    let elapsed = crate::bridge_time::now().duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(elapsed.as_millis()).ok()
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

/// Gateway close-code policy and the keepalive reply (live gateway, 2026-11-21).
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_4403_stops_after_one_refreshed_retry() {
        let mut retry = Retry::default();
        assert_eq!(retry.closed(Some(CLOSE_FORBIDDEN)), None);
        assert!(retry.fresh, "the retry refreshes the token");
        assert_eq!(retry.closed(Some(CLOSE_FORBIDDEN)), Some(Status::Unavailable));
    }

    #[test]
    fn close_4401_refreshes_and_gives_up_after_three() {
        let mut retry = Retry::default();
        for _ in 1..REFUSALS {
            assert_eq!(retry.closed(Some(CLOSE_UNAUTHORIZED)), None);
            assert!(retry.fresh);
        }
        assert_eq!(retry.closed(Some(CLOSE_UNAUTHORIZED)), Some(Status::Unavailable));
    }

    #[test]
    fn close_4503_backs_off_with_the_same_token() {
        let mut retry = Retry::default();
        let first = retry.pause();
        assert_eq!(retry.closed(Some(CLOSE_RETRY)), None);
        assert!(!retry.fresh, "4503 keeps the token");
        let second = retry.pause();
        assert!(second > first, "the backoff keeps growing: {first:?} then {second:?}");
        assert!(second >= Duration::from_secs(2));
        assert_eq!(retry.closed(Some(CLOSE_RETRY)), None);
        assert!(retry.pause() >= Duration::from_secs(4));
    }

    #[test]
    fn close_4000_and_4409_keep_their_meaning() {
        let mut retry = Retry::default();
        assert_eq!(retry.closed(Some(CLOSE_FORBIDDEN)), None);
        let _ = retry.pause();
        assert_eq!(retry.closed(Some(CLOSE_CYCLE)), None);
        assert!(!retry.fresh, "a cycle refreshes only a token near expiry");
        assert_eq!(retry.refusals, 0);
        assert_eq!(retry.delay, Duration::from_secs(1));
        assert_eq!(retry.closed(Some(CLOSE_REPLACED)), Some(Status::Unavailable));
    }

    #[test]
    fn only_the_exact_pong_is_a_keepalive_reply() {
        assert!(keepalive_reply("pong"));
        for text in ["ping", "PONG", "pong ", "{\"header\":{}}"] {
            assert!(!keepalive_reply(text), "{text}");
        }
        assert_eq!(PING, "ping");
        assert_eq!(KEEPALIVE, Duration::from_secs(45));
    }
}
