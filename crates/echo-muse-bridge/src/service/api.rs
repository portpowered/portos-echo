use super::{ipc::IO_TIMEOUT, state::Service};
use crate::Clock;
use axum::{
    body::{Body, to_bytes},
    http::{Request, Response, StatusCode, header},
};
use echo_agent_api::{AgentBackend, Task};
use echo_contracts::{ids::TaskId, secret::Secret};
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::net::TcpStream;
use tokio_rustls::TlsAcceptor;

pub(super) fn authorized(headers: &header::HeaderMap, credential: &Secret<Vec<u8>>) -> bool {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let Some(value) = values.next().and_then(|h| h.to_str().ok()) else {
        return false;
    };
    if values.next().is_some() {
        return false;
    }
    let Some(value) = value.strip_prefix("Bearer ") else {
        return false;
    };
    if value.len() > 256 {
        return false;
    }
    let mut supplied = [0; 256];
    let mut expected = [0; 256];
    supplied[..value.len()].copy_from_slice(value.as_bytes());
    expected[..credential.expose().len()].copy_from_slice(credential.expose());
    bool::from(supplied.ct_eq(&expected) & (value.len() as u64).ct_eq(&(credential.expose().len() as u64)))
}

pub(super) async fn serve(
    stream: TcpStream,
    tls: TlsAcceptor,
    service: Arc<Service>,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    let permit = Arc::new(permit);
    let Ok(Ok(stream)) = tokio::time::timeout(IO_TIMEOUT, tls.accept(stream)).await else {
        return;
    };
    let handler = service_fn(move |request| dispatch(request, service.clone(), permit.clone()));
    let mut builder = http1::Builder::new();
    builder
        .keep_alive(false)
        .max_buf_size(8192)
        .timer(TokioTimer::new())
        .header_read_timeout(IO_TIMEOUT);
    let _served = builder
        .serve_connection(TokioIo::new(super::transport::ResponseDeadline::new(stream)), handler)
        .await;
}

async fn dispatch(
    request: Request<Incoming>,
    service: Arc<Service>,
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
) -> Result<Response<Body>, Infallible> {
    if !authorized(request.headers(), &service.credential) {
        return Ok(error(401, "unauthorized"));
    }
    if service.stopping.load(Ordering::SeqCst) {
        return Ok(error(503, "shutting_down"));
    }
    if !service.store.healthy() {
        return Ok(error(503, "storage_unavailable"));
    }
    let (parts, incoming) = request.into_parts();
    let route = match route(parts.method.as_str(), parts.uri.path()) {
        Ok(route) => route,
        Err((status, code)) => return Ok(error(status, code)),
    };
    if matches!(route, Route::Submit)
        && parts
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .is_none_or(|h| h.split(';').next() != Some("application/json"))
    {
        return Ok(error(415, "unsupported_media_type"));
    }
    if parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.parse::<u64>().ok())
        .is_some_and(|size| size > 65536)
    {
        return Ok(error(413, "payload_too_large"));
    }
    let body = match tokio::time::timeout(IO_TIMEOUT, to_bytes(Body::new(incoming), 65536)).await {
        Err(_) => return Ok(error(408, "request_timeout")),
        Ok(Err(_)) => return Ok(error(413, "payload_too_large")),
        Ok(Ok(body)) => body,
    };
    Ok(execute(route, body.to_vec(), service, permit).await)
}

async fn execute(
    route: Route,
    body: Vec<u8>,
    service: Arc<Service>,
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
) -> Response<Body> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let worker_service = service.clone();
    let worker = tokio::spawn(async move {
        let _permit = permit;
        let result = operate(route, &body, &worker_service).await;
        let _delivered = sender.send(result);
    });
    match service.jobs.lock() {
        Ok(mut jobs) => {
            jobs.retain(|job| !job.is_finished());
            jobs.push(worker);
        }
        Err(_) => return error(500, "internal_error"),
    }
    let result = tokio::time::timeout(Duration::from_secs(4), receiver).await;
    // next_event intentionally exposes Option; the fail-stop store latch is the
    // independent witness distinguishing empty queues from failed transactions.
    if service.store.healthy() {
        match result {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => error(500, "internal_error"),
            Err(_) => error(504, "operation_timeout"),
        }
    } else {
        error(503, "storage_unavailable")
    }
}

enum Route {
    Submit,
    Events(TaskId),
    Cancel(TaskId),
}

fn route(method: &str, path: &str) -> Result<Route, (u16, &'static str)> {
    if path == "/v1/tasks" {
        return if method == "POST" {
            Ok(Route::Submit)
        } else {
            Err((405, "method_not_allowed"))
        };
    }
    let segments: Vec<_> = path.split('/').collect();
    if segments.len() != 5 || segments[1..3] != ["v1", "tasks"] || !["events", "cancel"].contains(&segments[4]) {
        return Err((404, "not_found"));
    }
    let id = TaskId::new(segments[3]).map_err(|_| (400, "invalid_request"))?;
    match (method, segments[4]) {
        ("GET", "events") => Ok(Route::Events(id)),
        ("POST", "cancel") => Ok(Route::Cancel(id)),
        _ => Err((405, "method_not_allowed")),
    }
}

async fn operate(route: Route, body: &[u8], service: &Service) -> Response<Body> {
    if !matches!(route, Route::Submit) && !body.is_empty() {
        return error(400, "invalid_request");
    }
    match route {
        Route::Submit => submit(body, service).await,
        Route::Events(id) => {
            if !matches!(service.store.contains(&id), Ok(true)) {
                return error(404, "unknown_task");
            }
            match tokio::time::timeout(Duration::from_secs(1), service.backend.next_event(&id)).await {
                Ok(Some(event)) => response(200, Some(serde_json::to_value(event).unwrap_or(Value::Null))),
                _ => response(204, None),
            }
        }
        Route::Cancel(id) => {
            if !matches!(service.store.contains(&id), Ok(true)) {
                return error(404, "unknown_task");
            }
            let Ok(_permit) = service.children.try_acquire() else {
                return error(503, "service_busy");
            };
            match service.backend.cancel(&id).await {
                Ok(()) => response(204, None),
                Err(_) => error(500, "internal_error"),
            }
        }
    }
}

async fn submit(body: &[u8], service: &Service) -> Response<Body> {
    let Ok(task) = serde_json::from_slice::<Task>(body) else {
        return error(400, "invalid_request");
    };
    if service.clock.now().checked_add(task.deadline_seconds).is_none() {
        return error(400, "invalid_request");
    }
    let Ok(_permit) = service.children.try_acquire() else {
        return error(503, "service_busy");
    };
    match service.backend.submit(task).await {
        Ok(id) => response(201, Some(json!({"task_id":id}))),
        Err(_) => error(500, "internal_error"),
    }
}

fn error(status: u16, code: &str) -> Response<Body> {
    response(status, Some(json!({"error":code})))
}

fn response(status: u16, value: Option<Value>) -> Response<Body> {
    let mut bytes = value.map_or_default(|value| value.to_string());
    let mut status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    if bytes.len() > 1_048_576 {
        bytes = "{\"error\":\"response_too_large\"}".into();
        status = StatusCode::SERVICE_UNAVAILABLE;
    }
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::CONNECTION, header::HeaderValue::from_static("close"));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    response
}
