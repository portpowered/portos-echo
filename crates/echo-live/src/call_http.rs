use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use serde_json::{Value, json};

use crate::call::{CallError, LiveAuthMode, LiveCredential, SessionOptions};

pub(crate) fn endpoint(base: &str, mode: LiveAuthMode) -> Result<reqwest::Url, CallError> {
    let mut url = reqwest::Url::parse(base).map_err(|_| CallError::Configuration)?;
    let secure = url.scheme() == "https";
    let local = url.scheme() == "http" && url.host_str() == Some("127.0.0.1");
    if !(secure || local)
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(CallError::Configuration);
    }
    match mode {
        LiveAuthMode::Chatgpt => {
            url.set_path("/backend-api/codex/realtime/calls");
            url.set_query(Some("intent=quicksilver&architecture=avas"));
        }
        LiveAuthMode::ApiKey => url.set_path("/v1/live/sessions"),
    }
    Ok(url)
}

pub(crate) fn validate_options(mode: LiveAuthMode, options: &SessionOptions) -> Result<(), CallError> {
    let model = match mode {
        LiveAuthMode::Chatgpt => "gpt-live-1-codex",
        LiveAuthMode::ApiKey => "gpt-live-1",
    };
    if options.model != model || options.voice.is_empty() || options.thread_id.is_empty() {
        return Err(CallError::Configuration);
    }
    value(&options.session_id.to_string())?;
    value(&options.thread_id)?;
    Ok(())
}

pub(crate) fn headers(
    mode: LiveAuthMode,
    credential: &LiveCredential,
    options: &SessionOptions,
) -> Result<HeaderMap, CallError> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("portos-echo/0.1.0"));
    let secret = match (mode, credential) {
        (LiveAuthMode::Chatgpt, LiveCredential::Chatgpt { token, account_id }) => {
            headers.insert("chatgpt-account-id", value(account_id)?);
            headers.insert("openai-alpha", HeaderValue::from_static("quicksilver=v2"));
            headers.insert("session-id", value(&options.session_id.to_string())?);
            headers.insert("x-session-id", value(&options.session_id.to_string())?);
            headers.insert("thread-id", value(&options.thread_id)?);
            headers.insert("originator", HeaderValue::from_static("portos-echo"));
            token
        }
        (LiveAuthMode::ApiKey, LiveCredential::ApiKey { key }) => key,
        _ => return Err(CallError::Configuration),
    };
    if secret.expose().is_empty() {
        return Err(CallError::Configuration);
    }
    let mut authorization = value(&format!("Bearer {}", secret.expose()))?;
    authorization.set_sensitive(true);
    headers.insert(AUTHORIZATION, authorization);
    Ok(headers)
}

fn value(text: &str) -> Result<HeaderValue, CallError> {
    if text.is_empty() {
        return Err(CallError::Configuration);
    }
    HeaderValue::from_str(text).map_err(|_| CallError::Configuration)
}

pub(crate) fn offer_body(sdp: &str, options: &SessionOptions) -> Value {
    json!({"sdp": sdp, "session": {"model": options.model, "voice": options.voice,
        "instructions": options.instructions, "delegation": {"target": "client"}}})
}

pub(crate) async fn answer(mut response: reqwest::Response) -> Result<String, CallError> {
    match response.status().as_u16() {
        401 | 403 => return Err(CallError::Authorization),
        500..=599 => return Err(CallError::Unavailable),
        200 => (),
        _ => return Err(CallError::Signaling),
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| CallError::Signaling)? {
        if bytes.len() + chunk.len() > 65_536 {
            return Err(CallError::Signaling);
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Err(crate::Error::InvalidSdp.into());
    }
    String::from_utf8(bytes).map_err(|_| crate::Error::InvalidSdp.into())
}
