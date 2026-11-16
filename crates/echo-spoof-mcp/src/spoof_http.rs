//! Loopback HTTP boundary, with no browser origins or session transport.
use crate::{CallRecord, Fault, RequestRecord, State, spoof_wire};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Request, State as AxumState},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

pub(crate) fn router(state: Arc<State>) -> Router {
    Router::new()
        .route("/mcp", post(handle))
        .route("/.well-known/oauth-protected-resource", get(metadata))
        .with_state(state)
}

async fn metadata(AxumState(state): AxumState<Arc<State>>, request: Request) -> Response {
    if request.headers().contains_key("origin") || request.headers().contains_key("dpop") {
        return StatusCode::FORBIDDEN.into_response();
    }
    state
        .validator
        .as_ref()
        .and_then(|validator| validator.protected_resource())
        .map_or_else(
            || StatusCode::NOT_FOUND.into_response(),
            |value| Json(value).into_response(),
        )
}

async fn handle(AxumState(state): AxumState<Arc<State>>, request: Request) -> Response {
    let Ok(_permit) = state.slots.try_acquire() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let mut stopped = state.stopped.clone();
    tokio::select! {
        response = tokio::time::timeout(Duration::from_secs(60), handle_request(&state, request)) =>
            response.unwrap_or_else(|_| StatusCode::SERVICE_UNAVAILABLE.into_response()),
        _ = stopped.wait_for(|v| *v) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn handle_request(state: &State, request: Request) -> Response {
    if request.headers().contains_key("origin") {
        return StatusCode::FORBIDDEN.into_response();
    }
    let equality = admission(state, &request);
    let session_header = request.headers().contains_key("mcp-session-id");
    let dpop_header = request.headers().contains_key("dpop");
    let fault = match select_fault(state, &request) {
        Ok(fault) => fault,
        Err(status) => return status.into_response(),
    };
    let Ok(bytes) = to_bytes(request.into_body(), 65_536).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if !spoof_wire::valid_envelope(&value) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if let Err(status) = record_request(state, &value, equality, session_header, dpop_header) {
        return status.into_response();
    }
    if equality == Some(false) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    dispatch(state, &value, fault).await
}

fn admission(state: &State, request: &Request<Body>) -> Option<bool> {
    let header = request.headers().get("authorization").and_then(|v| v.to_str().ok());
    let fixed = state
        .options
        .expected_bearer
        .as_ref()
        .map(|secret| header.is_some_and(|value| value == format!("Bearer {}", secret.expose())));
    state.validator.as_ref().map_or(fixed, |validator| {
        Some(
            fixed != Some(false)
                && !request.headers().contains_key("dpop")
                && header
                    .and_then(|value| value.strip_prefix("Bearer "))
                    .is_some_and(|value| validator.accepts(value)),
        )
    })
}

fn select_fault(state: &State, request: &Request<Body>) -> Result<Fault, StatusCode> {
    if let Some(value) = request.headers().get("x-echo-mcp-fault") {
        return value
            .to_str()
            .ok()
            .and_then(Fault::parse)
            .ok_or(StatusCode::BAD_REQUEST);
    }
    state
        .fault
        .lock()
        .map(|fault| *fault)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
}

fn record_request(
    state: &State,
    value: &Value,
    equality: Option<bool>,
    session: bool,
    dpop: bool,
) -> Result<(), StatusCode> {
    let mut records = state.records.lock().map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    if records.requests.len() >= state.options.max_records * 4 + 4 {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    records.requests.push(RequestRecord {
        method: value["method"].as_str().unwrap_or_default().into(),
        id: value["id"].as_u64(),
        authorization_matches: equality,
        session_header: session,
        dpop_header: dpop,
    });
    Ok(())
}

async fn dispatch(state: &State, value: &Value, fault: Fault) -> Response {
    let result = match value["method"].as_str() {
        Some("notifications/initialized") => return StatusCode::ACCEPTED.into_response(),
        Some("initialize") => json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},
            "serverInfo":{"name":"echo-spoof-mcp","version":"0.1.0"}}),
        Some("tools/list") => json!({"tools":spoof_wire::catalog()}),
        _ => return call(state, value, fault).await,
    };
    Json(spoof_wire::rpc(&value["id"], &result)).into_response()
}

async fn call(state: &State, value: &Value, fault: Fault) -> Response {
    let name = value["params"]["name"].as_str().unwrap_or_default();
    let args = &value["params"]["arguments"];
    if record_call(state, name, args).is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match fault {
        Fault::Http500 => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        Fault::MalformedJsonRpc => return Json(json!({"invalid":true})).into_response(),
        Fault::Slow => spoof_wire::wait_millis(state.options.slow_ms).await,
        Fault::None => {}
    }
    if !spoof_wire::valid_args(name, args) {
        return Json(spoof_wire::invalid_args(&value["id"])).into_response();
    }
    let result = spoof_wire::execute(name, args).await;
    Json(spoof_wire::rpc(&value["id"], &result)).into_response()
}

fn record_call(state: &State, name: &str, args: &Value) -> Result<(), ()> {
    let mut records = state.records.lock().map_err(|_| ())?;
    if records.calls.len() >= state.options.max_records {
        return Err(());
    }
    let count = records.calls.iter().filter(|record| record.name == name).count() as u64 + 1;
    records.calls.push(CallRecord {
        name: name.into(),
        args: args.clone(),
        count,
    });
    state.arrivals.send_replace(records.calls.len());
    Ok(())
}
