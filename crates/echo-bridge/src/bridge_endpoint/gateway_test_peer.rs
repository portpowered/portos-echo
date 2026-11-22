//! The endpoint T3 device gateway, as the live gateway README describes it:
//! any upgrade with a Bearer is accepted, and a token without the auth-link
//! claim (or no longer current) is closed at once with 4403. The device's text
//! `ping` is answered with `pong`, and every command frame sent to the device
//! opens one `requestId` that a single report may answer, unless the test says
//! no caller waits for it.
use echo_mcp_auth::mock::{Mock, State};
use futures_util::{SinkExt, StreamExt};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle, time::Instant};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{Callback, ErrorResponse, Request, Response},
        http::{Response as HttpResponse, StatusCode},
        protocol::{CloseFrame, frame::coding::CloseCode},
    },
};

pub(super) const PATH: &str = "/devices/loopback%20link%2F%25";

pub(super) struct Gateway {
    pub(super) template: String,
    pub(super) received: mpsc::Receiver<GatewayEvent>,
    pub(super) frames: mpsc::Sender<Message>,
    /// When each keepalive `ping` arrived.
    pub(super) pings: Pings,
    failures: Arc<AtomicUsize>,
    unwaited: Unwaited,
    task: JoinHandle<()>,
}

pub(super) type Pings = Arc<Mutex<Vec<Instant>>>;
/// Command ids no caller waits for: their frames open no `requestId`.
type Unwaited = Arc<Mutex<std::collections::BTreeSet<String>>>;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum GatewayEvent {
    Upgrade(String, String, bool),
    /// Closed with 4403: the Bearer had no auth-link claim or was not current.
    Forbidden(String),
    Disconnected,
    Rejected(Instant),
}

type Seen = Arc<Mutex<Option<(String, String, bool)>>>;

impl Gateway {
    pub(super) async fn open(mock: &Mock) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let template = format!(
            "ws://127.0.0.1:{}/devices/{{authLinkId}}", // lint-rules: allow-literal port interpolated from the loopback listener
            listener.local_addr().unwrap().port()
        );
        let (events, received) = mpsc::channel(8);
        let (frames, outgoing) = mpsc::channel(8);
        let failures = Arc::new(AtomicUsize::new(0));
        let pings = Pings::default();
        let unwaited = Unwaited::default();
        let peer = Peer {
            events,
            failures: failures.clone(),
            issuer: mock.state.clone(),
            pings: pings.clone(),
            unwaited: unwaited.clone(),
        };
        let task = tokio::spawn(peer.serve(listener, outgoing));
        Self {
            template,
            received,
            frames,
            pings,
            failures,
            unwaited,
            task,
        }
    }

    /// No caller waits for command `id`: a fire-and-forget request, or one
    /// past Port OS's 60 s. Its reply is answered 404.
    pub(super) fn unwaited(&self, id: &str) {
        self.unwaited.lock().unwrap().insert(id.into());
    }

    /// The next upgrade was admitted with this access token.
    pub(super) async fn assert_upgrade(&mut self, token: &str) {
        let wanted = GatewayEvent::Upgrade(PATH.into(), format!("Bearer {token}"), false);
        assert_eq!(self.received.recv().await, Some(wanted));
    }

    pub(super) async fn assert_disconnect(&mut self) {
        assert_eq!(self.received.recv().await, Some(GatewayEvent::Disconnected));
    }

    pub(super) fn reject_next(&self, count: usize) {
        self.failures.store(count, Ordering::SeqCst);
    }

    /// End the connection's hour: close 4000 (CYCLE).
    pub(super) async fn cycle(&self) {
        self.close(4000).await;
    }

    /// Close the live connection with a gateway close code.
    pub(super) async fn close(&self, code: u16) {
        let close = CloseFrame {
            code: CloseCode::from(code),
            reason: "synthetic".into(),
        };
        self.frames.send(Message::Close(Some(close))).await.unwrap();
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Peer {
    events: mpsc::Sender<GatewayEvent>,
    failures: Arc<AtomicUsize>,
    issuer: Arc<Mutex<State>>,
    pings: Pings,
    unwaited: Unwaited,
}

impl Peer {
    async fn serve(self, listener: TcpListener, mut frames: mpsc::Receiver<Message>) {
        while let Ok((stream, _)) = listener.accept().await {
            let seen = Seen::default();
            let upgrade = Upgrade(&self.events, &self.failures, seen.clone());
            let Ok(mut socket) = accept_hdr_async(stream, upgrade).await else {
                continue;
            };
            let Some((path, bearer, proof)) = seen.lock().unwrap().take() else {
                continue;
            };
            let token = bearer.strip_prefix("Bearer ").unwrap_or_default();
            if !self.issuer.lock().unwrap().admits_gateway(token) {
                let close = CloseFrame {
                    code: CloseCode::from(4403),
                    reason: "forbidden".into(),
                };
                let _ = socket.send(Message::Close(Some(close))).await;
                self.events.send(GatewayEvent::Forbidden(bearer)).await.unwrap();
                continue;
            }
            self.events
                .send(GatewayEvent::Upgrade(path, bearer, proof))
                .await
                .unwrap();
            self.connected(&mut socket, &mut frames).await;
            self.events.send(GatewayEvent::Disconnected).await.unwrap();
        }
    }
}

impl Peer {
    async fn connected<S>(
        &self,
        socket: &mut tokio_tungstenite::WebSocketStream<S>,
        frames: &mut mpsc::Receiver<Message>,
    ) where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        loop {
            tokio::select! {
                message = socket.next() => match message {
                    Some(Ok(Message::Text(text))) if text.as_str() == "ping" => {
                        self.pings.lock().unwrap().push(Instant::now());
                        if socket.send(Message::text("pong")).await.is_err() { break; }
                    }
                    Some(Ok(_)) => {}
                    _ => break,
                },
                Some(frame) = frames.recv() => {
                    self.open_request(&frame);
                    if socket.send(frame).await.is_err() { break; }
                }
            }
        }
    }

    /// Every command id a caller waits for may be answered once.
    fn open_request(&self, frame: &Message) {
        let Message::Text(text) = frame else { return };
        let Ok(command) = serde_json::from_str::<serde_json::Value>(text.as_str()) else {
            return;
        };
        if let Some(id) = command["header"]["id"].as_str()
            && !self.unwaited.lock().unwrap().contains(id)
        {
            self.issuer.lock().unwrap().endpoint.open_requests.insert(id.into());
        }
    }
}

struct Upgrade<'a>(&'a mpsc::Sender<GatewayEvent>, &'a AtomicUsize, Seen);

impl Callback for Upgrade<'_> {
    fn on_request(self, request: &Request, response: Response) -> Result<Response, ErrorResponse> {
        if self
            .1
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1))
            .is_ok()
        {
            self.0.try_send(GatewayEvent::Rejected(Instant::now())).unwrap();
            let mut refused = HttpResponse::new(None);
            *refused.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
            return Err(refused);
        }
        let bearer = request
            .headers()
            .get("Authorization")
            .and_then(|header| header.to_str().ok())
            .unwrap_or_default();
        *self.2.lock().unwrap() = Some((
            request.uri().path().into(),
            bearer.into(),
            request.headers().contains_key("DPoP"),
        ));
        Ok(response)
    }
}
