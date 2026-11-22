use super::{Camera, Connection, Event, MAX_JPEG_BYTES, Refused, wire};
use crate::{cancelled, draining, screen_http::HttpState};
use axum::{
    body::to_bytes,
    extract::{Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response, Sse, sse::KeepAlive},
};
use echo_contracts::secret::Secret;
use std::{convert::Infallible, time::Duration};
use tokio::sync::{mpsc, watch};

struct Stream {
    owner: Connection,
    key: Secret<[u8; 32]>,
    frames: mpsc::Receiver<Event>,
    revoke: watch::Receiver<bool>,
    state: HttpState,
    ready: bool,
}

pub(crate) async fn events(State(state): State<HttpState>, request: Request) -> Result<Response, StatusCode> {
    let camera = state.camera.clone().ok_or(StatusCode::NOT_FOUND)?;
    attach(&state, &camera, request).await.inspect_err(|status| {
        camera.refused(match *status {
            StatusCode::FORBIDDEN => Refused::Forbidden,
            StatusCode::CONFLICT => Refused::Conflict,
            _ => Refused::Unauthorized,
        });
    })
}

async fn attach(state: &HttpState, camera: &Camera, request: Request) -> Result<Response, StatusCode> {
    let (nonce, proof) = client(state, &request)?;
    let core = state.core.lock().await;
    let key = core.camera_key().ok_or(StatusCode::UNAUTHORIZED)?;
    if !wire::verify(key, nonce, &proof, &["attach"]) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let (owner, frames) = camera.attach(nonce.to_owned()).ok_or(StatusCode::CONFLICT)?;
    let stream = Stream {
        owner,
        frames,
        key: Secret::new(*key),
        revoke: core.revocation(),
        state: state.clone(),
        ready: true,
    };
    drop(core);
    let frames = futures_util::stream::unfold(stream, Stream::next);
    Ok(Sse::new(frames)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(5)).text("idle"))
        .into_response())
}

impl Stream {
    async fn next(mut self) -> Option<(Result<axum::response::sse::Event, Infallible>, Self)> {
        loop {
            if !self.owner.current()
                || *self.revoke.borrow()
                || *self.state.stop.borrow()
                || self.state.drain.borrow().is_some()
            {
                return None;
            }
            let next = if std::mem::take(&mut self.ready) {
                None
            } else {
                Some(tokio::select! {
                    biased;
                    () = cancelled(self.revoke.clone()) => return None,
                    () = cancelled(self.state.stop.clone()) => return None,
                    () = draining(self.state.drain.clone()) => return None,
                    event = self.frames.recv() => event?,
                })
            };
            if next.as_ref().is_some_and(|event| !self.owner.live(event)) {
                continue;
            }
            if let Some(frame) = wire::frame(self.key.expose(), &self.owner.nonce, next) {
                return Some((Ok(frame), self));
            }
        }
    }
}

pub(crate) async fn result(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    request: Request,
) -> Result<StatusCode, StatusCode> {
    let camera = state.camera.as_ref().ok_or(StatusCode::NOT_FOUND)?;
    let (nonce, proof) = client(&state, &request)?;
    let nonce = nonce.to_owned();
    if !wire::opaque_id(&id) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let (parts, body) = request.into_parts();
    if !camera.authorized_nonce(&nonce) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let metadata = wire::Metadata::read(&parts.headers)?;
    let core = state.core.lock().await;
    core.camera_key().ok_or(StatusCode::UNAUTHORIZED)?;
    let revoke = core.revocation();
    drop(core);
    let mut upload = camera.upload(&nonce, &id).ok_or(StatusCode::CONFLICT)?;
    let bytes = tokio::select! {
        biased;
        () = cancelled(revoke) => return Err(StatusCode::UNAUTHORIZED),
        () = cancelled(state.stop.clone()) => return Err(StatusCode::CONFLICT),
        () = draining(state.drain.clone()) => return Err(StatusCode::CONFLICT),
        _ = upload.retired.changed() => return Err(StatusCode::CONFLICT),
        () = tokio::time::sleep_until(upload.deadline) => return Err(StatusCode::CONFLICT),
        bytes = to_bytes(body, MAX_JPEG_BYTES) => bytes.map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?,
    };
    let core = state.core.lock().await;
    let key = core.camera_key().ok_or(StatusCode::UNAUTHORIZED)?;
    if !metadata.verify(key, &nonce, &proof, &id, &bytes) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if !upload.live {
        return Err(StatusCode::CONFLICT);
    }
    let result = metadata.result(bytes).ok_or(StatusCode::BAD_REQUEST)?;
    if !camera.accept(&nonce, &id, result) {
        return Err(StatusCode::CONFLICT);
    }
    Ok(StatusCode::NO_CONTENT)
}

fn client<'a>(state: &HttpState, request: &'a Request) -> Result<(&'a str, [u8; 32]), StatusCode> {
    let headers = request.headers();
    let authority = state.origin.strip_prefix("http://");
    if request.uri().query().is_some()
        || request.uri().authority().is_some()
        || headers.contains_key("origin")
        || headers.keys().any(|name| name.as_str().starts_with("sec-fetch-"))
        || wire::header(headers, "host").is_none_or(|host| Some(host) != authority)
    {
        return Err(StatusCode::FORBIDDEN);
    }
    let nonce = wire::header(headers, wire::NONCE)
        .filter(|value| wire::opaque_id(value))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let proof = wire::proof(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    Ok((nonce, proof))
}
