//! The endpoint T3 gateway accepts only synthetic endpoint tokens.
use futures_util::{SinkExt, StreamExt};
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{Callback, ErrorResponse, Request, Response},
        http::{Response as HttpResponse, StatusCode},
    },
};

pub(super) struct Gateway {
    pub(super) template: String,
    pub(super) received: mpsc::Receiver<GatewayEvent>,
    pub(super) frames: mpsc::Sender<Message>,
    task: JoinHandle<()>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum GatewayEvent {
    Upgrade(String, String, bool),
    Disconnected,
}

impl Gateway {
    pub(super) async fn open() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let template = format!(
            "ws://127.0.0.1:{}/devices/{{authLinkId}}", // lint-rules: allow-literal port interpolated from the loopback listener
            listener.local_addr().unwrap().port()
        );
        let (events, received) = mpsc::channel(8);
        let (frames, outgoing) = mpsc::channel(8);
        let task = tokio::spawn(gateway(listener, events, outgoing));
        Self {
            template,
            received,
            frames,
            task,
        }
    }

    pub(super) async fn assert_upgrade(&mut self, generation: usize) {
        let wanted = GatewayEvent::Upgrade(
            "/devices/loopback%20link%2F%25".into(),
            format!("Bearer endpoint-access-{generation}"),
            false,
        );
        assert_eq!(self.received.recv().await, Some(wanted));
    }

    pub(super) async fn assert_disconnect(&mut self) {
        assert_eq!(self.received.recv().await, Some(GatewayEvent::Disconnected));
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn gateway(listener: TcpListener, events: mpsc::Sender<GatewayEvent>, mut frames: mpsc::Receiver<Message>) {
    while let Ok((stream, _)) = listener.accept().await {
        let Ok(mut socket) = accept_hdr_async(stream, Upgrade(&events)).await else {
            continue;
        };
        loop {
            tokio::select! {
                message = socket.next() => if message.is_none_or(|frame| frame.is_err()) { break; },
                Some(frame) = frames.recv() => if socket.send(frame).await.is_err() { break; },
            }
        }
        events.send(GatewayEvent::Disconnected).await.unwrap();
    }
}

struct Upgrade<'a>(&'a mpsc::Sender<GatewayEvent>);

impl Callback for Upgrade<'_> {
    fn on_request(self, request: &Request, response: Response) -> Result<Response, ErrorResponse> {
        let bearer = request
            .headers()
            .get("Authorization")
            .and_then(|header| header.to_str().ok())
            .unwrap_or_default();
        if !bearer.starts_with("Bearer endpoint-access-") {
            let mut refused = HttpResponse::new(None);
            *refused.status_mut() = StatusCode::UNAUTHORIZED;
            return Err(refused);
        }
        self.0
            .try_send(GatewayEvent::Upgrade(
                request.uri().path().into(),
                bearer.into(),
                request.headers().contains_key("DPoP"),
            ))
            .unwrap();
        Ok(response)
    }
}
