use super::{HttpState, SettingsOperation, authorized};
use crate::{PromptKind, cancelled};
use axum::{
    body::to_bytes,
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use echo_screen_contract::{
    settings_v1::{PromptSaveV1, SettingsPatchV1},
    settings_v2::{PortosOperationV2, SettingsPatchV2},
};

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
    read(state, request, SettingsOperation::Snapshot).await
}

pub(super) async fn snapshot_v2(State(state): State<HttpState>, request: Request) -> Response {
    read(state, request, SettingsOperation::SnapshotV2).await
}

async fn read(state: HttpState, request: Request, operation: SettingsOperation) -> Response {
    if let Err(status) = guard(&state, request.uri(), request.headers()).await {
        return status.into_response();
    }
    state.settings_dispatch().send(request.uri().clone(), operation).await
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
    submit(state, request, 65_536, decode_v1).await
}

pub(super) async fn save_v2(State(state): State<HttpState>, request: Request) -> Response {
    submit(state, request, 65_536, decode_v2).await
}

pub(super) async fn portos_v2(State(state): State<HttpState>, request: Request) -> Response {
    submit(state, request, 65_536, |bytes| {
        serde_json::from_slice::<PortosOperationV2>(bytes)
            .ok()
            .map(SettingsOperation::PortosV2)
    })
    .await
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
    submit(state, request, 196_672, |bytes| {
        serde_json::from_slice::<PromptSaveV1>(bytes)
            .ok()
            .map(|body| SettingsOperation::Prompt(kind, body))
    })
    .await
}

async fn submit(
    state: HttpState,
    request: Request,
    limit: usize,
    decode: impl FnOnce(&[u8]) -> Option<SettingsOperation>,
) -> Response {
    if let Err(status) = guard(&state, request.uri(), request.headers()).await {
        return status.into_response();
    }
    if !json_media(&request) {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let uri = request.uri().clone();
    let bytes = tokio::select! {
        biased;
        () = cancelled(state.stop.clone()) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        () = crate::draining(state.drain.clone()) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        bytes = to_bytes(request.into_body(), limit) => bytes,
    };
    let Ok(bytes) = bytes else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let Some(operation) = decode(&bytes) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    state.settings_dispatch().send(uri, operation).await
}

fn decode_v1(bytes: &[u8]) -> Option<SettingsOperation> {
    let body: SettingsPatchV1 = serde_json::from_slice(bytes).ok()?;
    body.settings.is_nonempty().then_some(SettingsOperation::Save(body))
}

fn decode_v2(bytes: &[u8]) -> Option<SettingsOperation> {
    let body: SettingsPatchV2 = serde_json::from_slice(bytes).ok()?;
    body.settings.valid_shape().then_some(SettingsOperation::SaveV2(body))
}
