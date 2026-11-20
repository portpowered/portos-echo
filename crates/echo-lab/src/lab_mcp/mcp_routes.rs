use super::{Approval, Shared, State, mcp_proof, mcp_tokens};
use axum::{
    Json, Router,
    extract::{Form, State as Extract},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

type Reply = (StatusCode, Json<Value>);
type Fields = BTreeMap<String, String>;
const LINK: &str = "/custom/link";
const TOKEN: &str = "/custom/token";
const REVOKE: &str = "/custom/end";

pub(super) fn router(state: Shared) -> Router {
    Router::new()
        .route("/.well-known/oauth-protected-resource", get(resource))
        .route("/.well-known/oauth-authorization-server", get(metadata))
        .route(LINK, post(link))
        .route(TOKEN, post(token))
        .route(REVOKE, post(revoke))
        .with_state(state)
}

async fn resource(Extract(shared): Extract<Shared>, headers: HeaderMap) -> Reply {
    let Ok(state) = shared.lock() else {
        return unavailable();
    };
    if headers.contains_key("dpop") {
        return fail("invalid_request");
    }
    ok(json!({"resource":state.resource,"authorization_servers":[state.issuer]}))
}

async fn metadata(Extract(shared): Extract<Shared>, headers: HeaderMap) -> Reply {
    let Ok(state) = shared.lock() else {
        return unavailable();
    };
    if headers.contains_key("dpop") {
        return fail("invalid_request");
    }
    ok(
        json!({"issuer":state.issuer,"device_authorization_endpoint":format!("{}{LINK}",state.issuer),
        "token_endpoint":format!("{}{TOKEN}",state.issuer),"revocation_endpoint":format!("{}{REVOKE}",state.issuer),
        "dpop_signing_alg_values_supported":["ES256"]}),
    )
}

async fn link(Extract(shared): Extract<Shared>, headers: HeaderMap, Form(form): Form<Fields>) -> Reply {
    let Ok(mut state) = shared.lock() else {
        return unavailable();
    };
    if form.get("resource").is_some_and(|resource| resource != &state.resource) {
        return fail("invalid_target");
    }
    if !client(&form) || !form.get("scope").is_some_and(|scope| valid_scopes(scope)) {
        return fail("invalid_request");
    }
    if !proof(&mut state, &headers, LINK, true) {
        return fail("invalid_dpop_proof");
    }
    state.scopes.clone_from(&form["scope"]);
    state.device += 1;
    state.deadline = state.now.saturating_add(1800);
    state.bound = form.contains_key("resource");
    state.generation = 0;
    state.revoked = false;
    state.approval = Approval::Pending;
    state.used_refreshes.clear();
    state.counts.links += 1;
    ok(
        json!({"device_code":format!("synthetic-device-{}",state.device),"user_code":"TEST-CODE",
        "verification_uri":format!("{}/approve",state.issuer),"expires_in":1800,"interval":5}),
    )
}

async fn token(Extract(shared): Extract<Shared>, headers: HeaderMap, Form(form): Form<Fields>) -> Reply {
    let reply = token_reply(&shared, &headers, &form);
    #[cfg(test)]
    {
        let hold = shared.lock().ok().and_then(|state| state.token_reply.clone());
        if let Some(hold) = hold {
            hold.notified().await;
        }
    }
    reply
}

fn token_reply(shared: &Shared, headers: &HeaderMap, form: &Fields) -> Reply {
    let Ok(mut state) = shared.lock() else {
        return unavailable();
    };
    if !client(form) {
        return fail("invalid_client");
    }
    if !proof(&mut state, headers, TOKEN, false) {
        return fail("invalid_dpop_proof");
    }
    if state.revoked || state.device == 0 {
        return fail("invalid_grant");
    }
    match form.get("grant_type").map(String::as_str) {
        Some("urn:ietf:params:oauth:grant-type:device_code") => poll(&mut state, form),
        Some("refresh_token") => rotate(&mut state, form),
        _ => fail("unsupported_grant_type"),
    }
}

fn poll(state: &mut State, form: &Fields) -> Reply {
    state.counts.polls += 1;
    if form.get("device_code") != Some(&format!("synthetic-device-{}", state.device)) || state.generation != 0 {
        return fail("invalid_grant");
    }
    if state.now >= state.deadline {
        return fail("expired_token");
    }
    match state.approval {
        Approval::Pending => fail("authorization_pending"),
        Approval::Denied => fail("access_denied"),
        Approval::Approved => ok(mcp_tokens::pair(state)),
    }
}

fn rotate(state: &mut State, form: &Fields) -> Reply {
    let Some(refresh) = form.get("refresh_token") else {
        return fail("invalid_grant");
    };
    if state.used_refreshes.contains(refresh) {
        state.revoked = true;
        state.counts.replay_rejected += 1;
        return fail("invalid_grant");
    }
    if state.generation == 0 || refresh != &mcp_tokens::refresh(state) {
        return fail("invalid_grant");
    }
    state.used_refreshes.insert(refresh.clone());
    state.counts.refreshes += 1;
    ok(mcp_tokens::pair(state))
}

async fn revoke(Extract(shared): Extract<Shared>, headers: HeaderMap, Form(form): Form<Fields>) -> Reply {
    let Ok(mut state) = shared.lock() else {
        return unavailable();
    };
    if headers.contains_key("dpop") || !client(&form) {
        return fail("invalid_request");
    }
    if form
        .get("token")
        .is_some_and(|token| token == &mcp_tokens::refresh(&state) || state.used_refreshes.contains(token))
    {
        state.revoked = true;
        state.counts.revokes += 1;
    }
    ok(json!({}))
}

fn client(form: &Fields) -> bool {
    form.get("client_id").map(String::as_str) == Some("echo-lab")
}
fn proof(state: &mut State, headers: &HeaderMap, path: &str, register: bool) -> bool {
    headers
        .get("dpop")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| mcp_proof::verify(state, value, path, register))
}
fn ok(value: Value) -> Reply {
    (StatusCode::OK, Json(value))
}
fn fail(error: &str) -> Reply {
    (StatusCode::BAD_REQUEST, Json(json!({"error":error})))
}
fn unavailable() -> Reply {
    (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error":"unavailable"})))
}

fn valid_scopes(scope: &str) -> bool {
    let known = echo_config::McpConfig::default().scopes;
    let scopes: Vec<_> = scope.split(' ').collect();
    !scopes.is_empty() && scopes.iter().all(|scope| known.iter().any(|known| known == scope))
}
