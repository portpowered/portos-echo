//! Controlled external Live/model boundaries; all product execution stays in the bridge.
use std::sync::{Arc, Mutex};

use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use echo_live::LivePeer;
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle};

const ERROR: &str = "M5: controlled provider failed";
type Requests = Arc<Mutex<Vec<Value>>>;

pub struct Peers {
    pub origin: String,
    pub events: mpsc::Sender<String>,
    pub commentary: mpsc::Receiver<String>,
    pub requests: Requests,
    server: JoinHandle<Result<(), std::io::Error>>,
    peer: JoinHandle<Result<(), String>>,
}

impl Peers {
    pub async fn bind() -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|_| ERROR)?;
        let origin = format!("http://{}", listener.local_addr().map_err(|_| ERROR)?);
        let (send_peer, mut peers) = mpsc::channel::<LivePeer>(1);
        let (events, mut receive) = mpsc::channel::<String>(8);
        let (send, commentary) = mpsc::channel(8);
        let requests = Requests::default();
        let app = Router::new()
            .route("/v1/live/sessions", post(offer).with_state(send_peer))
            .route("/v1/responses", post(model).with_state(requests.clone()));
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let peer = tokio::spawn(async move {
            let mut peer = peers.recv().await.ok_or(ERROR)?;
            while !peer.ready() {
                drive(&mut peer).await?;
            }
            loop {
                tokio::select! {
                    event = receive.recv() => {
                        peer.send_text(&event.ok_or(ERROR)?).await.map_err(|_| ERROR)?;
                    }
                    result = drive(&mut peer) => result?,
                }
                while peer.pop_audio().is_some() {}
                while let Some(text) = peer.pop_text() {
                    send.try_send(text).map_err(|_| ERROR)?;
                }
            }
        });
        Ok(Self {
            origin,
            events,
            commentary,
            requests,
            server,
            peer,
        })
    }

    pub async fn shutdown(mut self) {
        self.server.abort();
        self.peer.abort();
        let _ = (&mut self.server).await;
        let _ = (&mut self.peer).await;
    }
}

impl Drop for Peers {
    fn drop(&mut self) {
        self.server.abort();
        self.peer.abort();
    }
}

async fn offer(State(send): State<mpsc::Sender<LivePeer>>, Json(body): Json<Value>) -> Result<String, StatusCode> {
    let mut peer = Box::pin(LivePeer::bind(
        ([127, 0, 0, 1], 0).into(),
        tokio::time::Instant::now().into_std(),
    ))
    .await
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let answer = peer
        .answer(body["sdp"].as_str().ok_or(StatusCode::BAD_REQUEST)?)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    send.try_send(peer).map_err(|_| StatusCode::CONFLICT)?;
    Ok(answer)
}

async fn model(
    State(requests): State<Requests>,
    Json(body): Json<Value>,
) -> Result<([(axum::http::HeaderName, &'static str); 1], String), StatusCode> {
    let mut requests = requests.lock().map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let index = requests.len();
    if index == 128 {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    requests.push(body);
    let item = match index {
        0 | 2 => json!({"type":"function_call","call_id":format!("m5-call-{index}"),
            "name":"tool_0","arguments":"{\"text\":\"m5-linked\"}"}),
        1 => json!({"type":"message","role":"assistant",
            "content":[{"type":"output_text","text":"m5-linked"}]}),
        _ => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };
    let sse = format!(
        "data: {}\n\ndata: {}\n\n",
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        json!({"type":"response.completed","response":{"status":"completed"}})
    );
    Ok(([(axum::http::header::CONTENT_TYPE, "text/event-stream")], sse))
}

async fn drive(peer: &mut LivePeer) -> Result<(), String> {
    let deadline = peer.flush().await.map_err(|_| ERROR)?;
    tokio::select! {
        result = peer.readable() => {
            result.map_err(|_| ERROR)?;
            peer.receive(tokio::time::Instant::now().into_std()).await.map_err(|_| ERROR)?;
        }
        () = tokio::time::sleep_until(deadline.into()) => {
            peer.timeout(tokio::time::Instant::now().into_std()).await.map_err(|_| ERROR)?;
        }
    }
    Ok(())
}
