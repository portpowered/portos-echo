use crate::{
    cancelled, screen_assets,
    screen_core::Core,
    screen_settings::{SettingsDispatch, SettingsOperation, SettingsRequest},
};
#[path = "screen_settings_http.rs"]
mod settings_http;
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
pub(super) struct HttpState {
    pub(crate) core: Arc<Mutex<Core>>,
    actions: mpsc::Sender<UiAction>,
    pub(crate) stop: watch::Receiver<bool>,
    pub(crate) drain: watch::Receiver<Option<tokio::time::Instant>>,
    pub(crate) packs: Arc<crate::packs::Files>,
    pub(crate) origin: String,
    settings: Option<mpsc::Sender<SettingsRequest>>,
}

pub(crate) fn router(
    core: Arc<Mutex<Core>>,
    actions: mpsc::Sender<UiAction>,
    stop: watch::Receiver<bool>,
    drain: watch::Receiver<Option<tokio::time::Instant>>,
    addr: SocketAddr,
    settings: Option<mpsc::Sender<SettingsRequest>>,
    packs: Arc<crate::packs::Files>,
) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/screen.css", get(screen_assets::css))
        .route("/screen.js", get(screen_assets::js))
        .route("/events", get(events))
        .route("/actions", post(actions_post))
        .route("/settings/v1", get(settings_http::snapshot).post(settings_http::save))
        .route("/settings/v1/prompts/{kind}", post(settings_http::prompt))
        .route("/screen-packs/v1", get(crate::packs::view::snapshot))
        .route("/screen-packs/v1/install", post(crate::packs::jobs::install))
        .route("/screen-packs/v1/selection", post(crate::packs::view::selection))
        .route("/screen-packs/v1/{pack}/{*file}", get(crate::packs::http::file))
        .layer(middleware::from_fn(headers))
        .with_state(HttpState {
            core,
            actions,
            stop,
            drain,
            origin: format!("http://{addr}"),
            settings,
            packs,
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

pub(crate) fn authorized(core: &Core, uri: &Uri) -> bool {
    let Ok(Query(pairs)) = Query::<Vec<(String, String)>>::try_from_uri(uri) else {
        return false;
    };
    let mut tokens = pairs.iter().filter(|(name, _)| name == "token");
    let Some((_, token)) = tokens.next() else {
        return false;
    };
    tokens.next().is_none() && core.authorized(token)
}

async fn index(State(state): State<HttpState>) -> Response {
    screen_assets::index_with_settings(state.settings.is_some())
}

impl HttpState {
    pub(crate) fn settings_dispatch(&self) -> SettingsDispatch {
        SettingsDispatch {
            core: self.core.clone(),
            stop: self.stop.clone(),
            drain: self.drain.clone(),
            host: self.settings.clone(),
        }
    }
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
    let frames = stream::unfold(
        (client, state.stop, state.drain),
        |(mut client, stop, drain)| async move {
            if drain.borrow().is_some() {
                client.frames.close();
            }
            tokio::select! {
                biased;
                () = cancelled(stop.clone()) => None,
                () = cancelled(client.cancelled.clone()) => None,
                () = crate::draining(drain.clone()), if !client.frames.is_closed() => {
                    client.frames.close();
                    client.frames.recv().await.map(|wire| (Ok::<_, Infallible>(Event::default().data(wire)), (client, stop, drain)))
                },
                frame = client.frames.recv() => frame.map(|wire| (Ok::<_, Infallible>(Event::default().data(wire)), (client, stop, drain))),
            }
        },
    );
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

#[cfg(test)]
mod tests {
    use super::*;
    use echo_contracts::secret::Secret;
    use echo_screen_contract::UiState;
    use futures_util::StreamExt;

    #[tokio::test]
    async fn queued_terminal_frame_drains_before_eof() {
        let core = Arc::new(Mutex::new(Core::new(&Secret::new("synthetic-screen-pair".into()))));
        let (_stop, stop_rx) = watch::channel(false);
        let (drain, drain_rx) = watch::channel(None);
        let state = HttpState {
            core: core.clone(),
            actions: mpsc::channel(1).0,
            stop: stop_rx,
            drain: drain_rx,
            origin: "http://127.0.0.1:1".into(),
            settings: None,
            packs: Arc::default(),
        };
        let request = Request::builder()
            .uri("/events?token=synthetic-screen-pair")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = events(State(state), Extension(watch::channel(false).0), request).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(core.lock().await.publish(UiState::Error {
            message: "synthetic terminal".into(),
            relink: false,
        }));
        core.lock().await.drain();
        drain.send_replace(Some(tokio::time::Instant::now()));
        let mut body = response.into_body().into_data_stream();
        let frame = body.next().await.unwrap().unwrap();
        assert_eq!(
            std::str::from_utf8(&frame).unwrap(),
            "data: {\"seq\":1,\"state\":{\"state\":\"error\",\"message\":\"synthetic terminal\",\"relink\":false}}\n\n"
        );
        assert!(body.next().await.is_none());
    }

    #[tokio::test]
    async fn pending_terminal_frame_is_discarded_on_revocation_or_cancellation() {
        for revoked in [true, false] {
            let core = Arc::new(Mutex::new(Core::new(&Secret::new("synthetic-screen-pair".into()))));
            let (stop, stop_rx) = watch::channel(false);
            let (drain, drain_rx) = watch::channel(None);
            let state = HttpState {
                core: core.clone(),
                actions: mpsc::channel(1).0,
                stop: stop_rx,
                drain: drain_rx,
                origin: "http://127.0.0.1:1".into(),
                settings: None,
                packs: Arc::default(),
            };
            let request = Request::builder()
                .uri("/events?token=synthetic-screen-pair")
                .body(axum::body::Body::empty())
                .unwrap();
            let response = events(State(state), Extension(watch::channel(false).0), request).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert!(core.lock().await.publish(UiState::Listening));
            core.lock().await.drain();
            drain.send_replace(Some(tokio::time::Instant::now()));
            if revoked {
                core.lock().await.revoke();
            } else {
                stop.send_replace(true);
            }
            assert!(response.into_body().into_data_stream().next().await.is_none());
        }
    }
}
