use super::{catalog, css, extension, relative};
use crate::screen_http::{HttpState, authorized};
use axum::{
    extract::{Query, Request, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
};

fn guard(state: &HttpState, uri: &Uri) -> Result<(), StatusCode> {
    if *state.stop.borrow() || state.drain.borrow().is_some() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    if state.packs.root.get().is_none() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    if uri.path().contains('%') {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(())
}

pub(crate) async fn file(State(state): State<HttpState>, request: Request) -> Response {
    if !authorized(&*state.core.lock().await, request.uri()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if let Err(status) = guard(&state, request.uri()) {
        return status.into_response();
    }
    let Some((pack, file)) = request
        .uri()
        .path()
        .strip_prefix("/screen-packs/v1/")
        .and_then(|path| path.split_once('/'))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if relative(file).is_err() || extension(file).is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut files = match read(&state, request.uri(), pack).await {
        Ok(files) => files,
        Err(status) => return status.into_response(),
    };
    let core = state.core.lock().await;
    if !authorized(&core, request.uri()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !core.accepting() || *state.stop.borrow() || state.drain.borrow().is_some() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let Some(mut bytes) = files.remove(file) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if extension(file) == Ok("css") {
        let Ok(rendered) = stylesheet(&bytes, request.uri(), pack) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        bytes = rendered;
    }
    response(bytes, file, request.headers())
}

// HTTP/1 runs one handler per connection; the listener already bounds the
// queue to 128 connections. Keep one off-runtime reader, with cancellable FIFO
// admission rather than refusing a browser's parallel CSS/media/preview reads.
pub(super) async fn permit(state: &HttpState, uri: &Uri) -> Result<tokio::sync::OwnedMutexGuard<()>, StatusCode> {
    #[cfg(test)]
    state.packs.waiting.notify_one();
    let permit = tokio::select! {
        biased;
        () = crate::cancelled(state.stop.clone()) => Err(StatusCode::SERVICE_UNAVAILABLE),
        () = crate::draining(state.drain.clone()) => Err(StatusCode::SERVICE_UNAVAILABLE),
        permit = state.packs.busy.clone().lock_owned() => Ok(permit),
    }?;
    let core = state.core.lock().await;
    if !authorized(&core, uri) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if !core.accepting() || *state.stop.borrow() || state.drain.borrow().is_some() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    Ok(permit)
}

async fn read(
    state: &HttpState,
    uri: &Uri,
    identity: &str,
) -> Result<std::collections::BTreeMap<String, Vec<u8>>, StatusCode> {
    let permit = permit(state, uri).await?;
    let root = state.packs.root.get().cloned().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let identity = identity.to_owned();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        catalog::read(&root, &identity)
    })
    .await
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
    .map_err(|_| StatusCode::NOT_FOUND)
}

fn stylesheet(bytes: &[u8], uri: &Uri, pack: &str) -> Result<Vec<u8>, ()> {
    let Query(pairs) = Query::<Vec<(String, String)>>::try_from_uri(uri).map_err(|_| ())?;
    let token = pairs.iter().find(|(key, _)| key == "token").ok_or(())?;
    let mut url = reqwest::Url::parse("http://127.0.0.1").map_err(|_| ())?;
    url.query_pairs_mut().append_pair("token", &token.1);
    let query = url.query().ok_or(())?;
    let text = std::str::from_utf8(bytes).map_err(|_| ())?;
    css::render(text, &format!("/screen-packs/v1/{pack}/"), query)
        .map(String::into_bytes)
        .map_err(|_| ())
}

fn response(bytes: Vec<u8>, file: &str, headers: &HeaderMap) -> Response {
    let total = bytes.len();
    let range = if headers.get_all("range").iter().count() > 1 {
        Err(())
    } else {
        headers.get("range").map_or(Ok(None), |value| {
            value
                .to_str()
                .map_err(|_| ())
                .and_then(|value| range(value, total))
                .map(Some)
        })
    };
    let (status, body, content_range) = match range {
        Ok(None) => (StatusCode::OK, bytes, None),
        Ok(Some((start, end))) => (
            StatusCode::PARTIAL_CONTENT,
            bytes[start..=end].to_vec(),
            Some(format!("bytes {start}-{end}/{total}")),
        ),
        Err(()) => (
            StatusCode::RANGE_NOT_SATISFIABLE,
            vec![],
            Some(format!("bytes */{total}")),
        ),
    };
    let mut builder = Response::builder()
        .status(status)
        .header("content-type", mime(file))
        .header("x-content-type-options", "nosniff")
        .header("accept-ranges", "bytes")
        .header("content-length", body.len());
    if let Some(value) = content_range {
        builder = builder.header("content-range", value);
    }
    builder
        .body(body.into())
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn range(value: &str, total: usize) -> Result<(usize, usize), ()> {
    let value = value.strip_prefix("bytes=").ok_or(())?;
    let (start, end) = value.split_once('-').ok_or(())?;
    let decimal = |text: &str| -> Result<usize, ()> {
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(());
        }
        text.parse().map_err(|_| ())
    };
    if total == 0 {
        return Err(());
    }
    if start.is_empty() {
        let suffix = decimal(end)?;
        return if suffix == 0 {
            Err(())
        } else {
            Ok((total.saturating_sub(suffix), total - 1))
        };
    }
    let start = decimal(start)?;
    let end = if end.is_empty() {
        total - 1
    } else {
        decimal(end)?.min(total - 1)
    };
    if start >= total || end < start {
        return Err(());
    }
    Ok((start, end))
}

fn mime(file: &str) -> &'static str {
    match extension(file) {
        Ok("json") => "application/json",
        Ok("css") => "text/css",
        Ok("png") => "image/png",
        Ok("webp") => "image/webp",
        Ok("webm") => "video/webm",
        Ok("mp4") => "video/mp4",
        _ => "application/octet-stream",
    }
}
