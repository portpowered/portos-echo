use crate::screen_http::{HttpState, authorized};
use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

type Exchange = dyn Fn(Vec<u8>) -> Pin<Box<dyn Future<Output = Option<Vec<u8>>> + Send>> + Send + Sync;

/// The runtime's relay for one Device Wi-Fi v1 request. An empty result means
/// the frame was invalid and did not reach the device.
#[derive(Clone)]
pub struct WifiRelay(Arc<Exchange>);

impl WifiRelay {
    /// Attach the device-socket adapter to the screen listener.
    pub fn new<F, Fut>(exchange: F) -> Self
    where
        F: Fn(Vec<u8>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Option<Vec<u8>>> + Send + 'static,
    {
        Self(Arc::new(move |body| Box::pin(exchange(body))))
    }
}

pub(crate) async fn post(State(state): State<HttpState>, request: Request) -> Response {
    match submit(state, request).await {
        Ok(reply) => ([(header::CONTENT_TYPE, "application/json")], reply).into_response(),
        Err(status) => status.into_response(),
    }
}

async fn submit(state: HttpState, request: Request) -> Result<Vec<u8>, StatusCode> {
    {
        let core = state.core.lock().await;
        if !authorized(&core, request.uri()) {
            return Err(StatusCode::UNAUTHORIZED);
        }
        let mut origins = request.headers().get_all(header::ORIGIN).iter();
        if origins.next().is_none_or(|origin| origin != state.origin.as_str()) || origins.next().is_some() {
            return Err(StatusCode::FORBIDDEN);
        }
        if *state.stop.borrow() || state.drain.borrow().is_some() || !core.accepting() {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
    }
    let relay = state.wifi.ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    if !request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
        })
    {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let (parts, body) = request.into_parts();
    let body = tokio::time::timeout(Duration::from_secs(2), read_body(body))
        .await
        .map_err(|_| StatusCode::REQUEST_TIMEOUT)??;
    {
        let core = state.core.lock().await;
        if !authorized(&core, &parts.uri) {
            return Err(StatusCode::UNAUTHORIZED);
        }
        if *state.stop.borrow() || state.drain.borrow().is_some() || !core.accepting() {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
    }
    relay.0(body).await.ok_or(StatusCode::BAD_REQUEST)
}

async fn read_body(body: Body) -> Result<Vec<u8>, StatusCode> {
    let mut bytes = Vec::new();
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk: Bytes = chunk.map_err(|_| StatusCode::BAD_REQUEST)?;
        if chunk.len() > 1024 - bytes.len() {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
