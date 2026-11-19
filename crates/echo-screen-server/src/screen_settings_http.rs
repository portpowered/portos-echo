use super::{HttpState, SettingsOperation, authorized};
use crate::{PromptKind, cancelled};
use axum::{
    body::to_bytes,
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use echo_screen_contract::settings_v1::{PromptSaveV1, SettingsPatchV1};

async fn guard(state: &HttpState, uri: &Uri, headers: &HeaderMap) -> Result<(), StatusCode> {
    let core = state.core.lock().await;
    if !authorized(&core, uri) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if headers
        .get("origin")
        .is_some_and(|origin| origin != state.origin.as_str())
    {
        return Err(StatusCode::FORBIDDEN);
    }
    if *state.stop.borrow() || state.drain.borrow().is_some() || !core.accepting() || state.settings.is_none() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    Ok(())
}

pub(super) async fn snapshot(State(state): State<HttpState>, request: Request) -> Response {
    if let Err(status) = guard(&state, request.uri(), request.headers()).await {
        return status.into_response();
    }
    state
        .settings_dispatch()
        .send(request.uri().clone(), SettingsOperation::Snapshot)
        .await
}

fn json_media(request: &Request) -> bool {
    request
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
        })
}

pub(super) async fn save(State(state): State<HttpState>, request: Request) -> Response {
    submit(state, request, None).await
}

pub(super) async fn prompt(State(state): State<HttpState>, Path(kind): Path<String>, request: Request) -> Response {
    if let Err(status) = guard(&state, request.uri(), request.headers()).await {
        return status.into_response();
    }
    let kind = match kind.as_str() {
        "voice" => PromptKind::Voice,
        "runner" => PromptKind::Runner,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    submit(state, request, Some(kind)).await
}

async fn submit(state: HttpState, request: Request, kind: Option<PromptKind>) -> Response {
    if let Err(status) = guard(&state, request.uri(), request.headers()).await {
        return status.into_response();
    }
    if !json_media(&request) {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let uri = request.uri().clone();
    let limit = if kind.is_some() { 196_672 } else { 65_536 };
    let bytes = tokio::select! {
        biased;
        () = cancelled(state.stop.clone()) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        () = crate::draining(state.drain.clone()) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        bytes = to_bytes(request.into_body(), limit) => bytes,
    };
    let Ok(bytes) = bytes else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let Some(operation) = decode(&bytes, kind) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    state.settings_dispatch().send(uri, operation).await
}

fn decode(bytes: &[u8], kind: Option<PromptKind>) -> Option<SettingsOperation> {
    if let Some(kind) = kind {
        return serde_json::from_slice::<PromptSaveV1>(bytes)
            .ok()
            .map(|body| SettingsOperation::Prompt(kind, body));
    }
    let body: SettingsPatchV1 = serde_json::from_slice(bytes).ok()?;
    body.settings.is_nonempty().then_some(SettingsOperation::Save(body))
}
