use crate::{cancelled, screen_assets, screen_core::Core};
use axum::{
    Extension, Router,
    body::to_bytes,
    extract::{Query, Request, State},
    http::{StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response, Sse, sse::Event},
    routing::{get, post},
};
use echo_screen_contract::UiAction;
use futures_util::stream;
use std::{convert::Infallible, net::SocketAddr, sync::Arc};
use tokio::sync::{Mutex, mpsc, watch};

#[derive(Clone)]
struct HttpState {
    core: Arc<Mutex<Core>>,
    actions: mpsc::Sender<UiAction>,
    stop: watch::Receiver<bool>,
    origin: String,
}

pub(crate) fn router(
    core: Arc<Mutex<Core>>,
    actions: mpsc::Sender<UiAction>,
    stop: watch::Receiver<bool>,
    addr: SocketAddr,
) -> Router {
    Router::new()
        .route("/", get(screen_assets::index))
        .route("/screen.css", get(screen_assets::css))
        .route("/screen.js", get(screen_assets::js))
        .route("/events", get(events))
        .route("/actions", post(actions_post))
        .layer(middleware::from_fn(headers))
        .with_state(HttpState {
            core,
            actions,
            stop,
            origin: format!("http://{addr}"),
        })
}

async fn headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert("cache-control", axum::http::HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert("referrer-policy", axum::http::HeaderValue::from_static("no-referrer"));
    response
}

fn authorized(core: &Core, uri: &Uri) -> bool {
    let Ok(Query(pairs)) = Query::<Vec<(String, String)>>::try_from_uri(uri) else {
        return false;
    };
    let mut tokens = pairs.iter().filter(|(name, _)| name == "token");
    let Some((_, token)) = tokens.next() else {
        return false;
    };
    tokens.next().is_none() && core.authorized(token)
}

async fn events(
    State(state): State<HttpState>,
    Extension(cancel): Extension<watch::Sender<bool>>,
    request: Request,
) -> Response {
    let mut core = state.core.lock().await;
    if !authorized(&core, request.uri()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if *state.stop.borrow() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let Some(client) = core.attach(cancel) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    drop(core);
    let frames = stream::unfold((client, state.stop), |(mut client, stop)| async move {
        tokio::select! {
            biased;
            () = cancelled(stop.clone()) => None,
            () = cancelled(client.cancelled.clone()) => None,
            frame = client.frames.recv() => frame.map(|wire| (Ok::<_, Infallible>(Event::default().data(wire)), (client, stop))),
        }
    });
    Sse::new(frames).into_response()
}

async fn actions_post(State(state): State<HttpState>, request: Request) -> Response {
    {
        let core = state.core.lock().await;
        if !authorized(&core, request.uri()) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }
    if request
        .headers()
        .get("origin")
        .is_some_and(|origin| origin != state.origin.as_str())
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !request
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
        })
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let uri = request.uri().clone();
    let bytes = tokio::select! {
        biased;
        () = cancelled(state.stop.clone()) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        bytes = to_bytes(request.into_body(), 65536) => bytes,
    };
    let Ok(bytes) = bytes else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let Ok(action) = serde_json::from_slice::<UiAction>(&bytes) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let core = state.core.lock().await;
    if !authorized(&core, &uri) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if *state.stop.borrow() || state.actions.try_send(action).is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}
