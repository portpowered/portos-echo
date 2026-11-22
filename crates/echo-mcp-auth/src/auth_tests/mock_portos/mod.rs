//! The live Port OS contract (2026-11-21) as the loopback fake enforces it:
//! two RFC 8707 audiences from one sign-in, scopes per REST route and per MCP
//! tool, the pairing exchange, route read-back and strict `report-state` bodies.
use super::{Request, State, wall_now};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

mod report;
use report::envelope;

/// The live Port OS contract, enforced once `enable_endpoint_audience` is called.
#[derive(Default)]
pub struct EndpointState {
    pub(super) access: Option<u64>,
    pub(super) enabled: bool,
    pub(super) exchanges: BTreeMap<String, Option<Value>>,
    pub mcp_claimed: bool,
    pub endpoint_claimed: bool,
    /// Access tokens minted by a refresh after pairing: they carry the auth-link claim.
    pub claimed: BTreeSet<String>,
    /// The resources the current device authorization approved, in request order.
    pub resources: Vec<String>,
    /// The scope the owner approved; both audiences carry it.
    pub scope: String,
    /// The scope the DCR client registered, when one registered.
    pub registered_scope: Option<String>,
    /// The audience a refresh without `resource` keeps.
    last_audience: Option<String>,
    /// The deviceId the pairing exchange named.
    pub device_id: Option<String>,
    /// Route discovery lags pairing: this many `/routes` reads omit the Echo; its state reports get 404 until then.
    pub route_delay: usize,
    /// Other owner routes listed before the Echo's, to exercise pagination.
    pub route_padding: usize,
    /// This many `/routes` reads that carry a `nextToken` get 400, as an expired continuation does.
    pub route_token_rejections: usize,
    /// List first a route an old pairing left for the same deviceId under this auth link.
    pub stale_link: Option<&'static str>,
    /// Command ids one report may still answer; any other `requestId` gets 404.
    pub open_requests: BTreeSet<String>,
    /// Report idempotency keys already taken.
    pub report_keys: BTreeSet<String>,
    /// Why each refused report was refused, for diagnostics.
    pub refusals: Vec<String>,
}

const PLUGIN_ID: &str = "port1/systems/zero/plugins/15badf8f-fe9b-4bf0-be33-2d7eb287f89f";
/// The auth link id the pairing exchange answers with.
pub const AUTH_LINK: &str = "loopback link/%";
/// The endpoint id route read-back returns for the paired Echo.
pub const ECHO_ENDPOINT_ID: &str = "synthetic-echo-endpoint";
/// The auth link and endpoint of a route an old pairing left for the same deviceId.
pub const STALE_LINK: &str = "synthetic-stale-link";
pub const STALE_ENDPOINT_ID: &str = "synthetic-stale-endpoint";
/// Another owner device whose route `/routes` also lists.
pub const OTHER_DEVICE: &str = "synthetic-other-device";
/// `/routes` page sizes: the default and the largest `maxResults`.
const ROUTE_PAGE: usize = 10;
const MAX_ROUTE_PAGE: usize = 100;
/// The opaque continuation prefix; it needs query encoding on purpose.
pub const ROUTE_TOKEN: &str = "synthetic+token/=";

/// Remember the scope a DCR registration asked for; the fault answers it.
pub(super) fn observe_registration(request: &Request, state: &mut State) {
    let registration = state
        .metadata
        .as_ref()
        .and_then(|metadata| metadata["registration_endpoint"].as_str())
        .is_some_and(|endpoint| endpoint.ends_with(&request.path));
    if registration && let Some(scope) = request.json.as_ref().and_then(|body| body["scope"].as_str()) {
        state.endpoint.registered_scope = Some(scope.into());
    }
}

fn resources(request: &Request) -> Vec<String> {
    request
        .form_all
        .iter()
        .filter(|(name, _)| name == "resource")
        .map(|(_, value)| value.clone())
        .collect()
}

fn scopes(text: &str) -> BTreeSet<&str> {
    text.split_ascii_whitespace().collect()
}

/// Device authorization: both audiences, repeated, and a registered scope.
pub(super) fn authorize(request: &Request, base: &str, state: &mut State) -> Result<(), &'static str> {
    let asked = resources(request);
    let both = [base.to_owned(), format!("{base}/mcp")];
    if asked.len() != 2 || both.iter().any(|resource| !asked.contains(resource)) {
        return Err("invalid_target");
    }
    let scope = request.form.get("scope").cloned().unwrap_or_default();
    if let Some(registered) = &state.endpoint.registered_scope
        && !scopes(&scope).is_subset(&scopes(registered))
    {
        return Err("invalid_scope");
    }
    state.endpoint.resources = asked;
    state.endpoint.scope = scope;
    Ok(())
}

/// The audience of a token request: true for the REST audience.
pub(super) fn audience(request: &Request, base: &str, state: &mut State, refresh: bool) -> Result<bool, &'static str> {
    if !state.endpoint.enabled {
        return Ok(false);
    }
    let asked = resources(request);
    let chosen = match asked.as_slice() {
        [] if refresh => state.endpoint.last_audience.clone().unwrap_or_default(),
        [] => state.endpoint.resources.first().cloned().unwrap_or_default(),
        [one] if state.endpoint.resources.contains(one) => one.clone(),
        _ => return Err("invalid_target"),
    };
    let rest = chosen == base;
    state.endpoint.last_audience = Some(chosen);
    Ok(rest)
}

pub(super) fn admits_gateway(state: &State, bearer: &str) -> bool {
    let current = [
        format!("access-{}", state.mcp_access),
        format!("endpoint-access-{}", state.endpoint.access.unwrap_or_default()),
    ];
    !state.revoked && state.endpoint.claimed.contains(bearer) && current.iter().any(|token| token == bearer)
}

fn insufficient(state: &State, needed: &str) -> Option<(u16, Value)> {
    (!scopes(&state.endpoint.scope).contains(needed))
        .then(|| (403, json!({"error":"insufficient_scope","scope":needed})))
}

/// `/mcp` admits by audience (checked by the caller), then by the tool's scope.
pub(super) fn tool_scope(request: &Request, state: &State) -> (u16, Value) {
    let body = request.json.as_ref();
    let method = body.and_then(|body| body["method"].as_str()).unwrap_or_default();
    let tool = body
        .and_then(|body| body["params"]["name"].as_str())
        .unwrap_or_default();
    let needed = match (method, tool) {
        ("tools/call", "control") => Some("message:send"),
        ("tools/call", "endpoint_query" | "endpoint_show") => Some("endpoint:read"),
        ("tools/call", "endpoint_manage") => Some("endpoint:write"),
        ("tools/call", "group_query") => Some("group:read"),
        ("tools/call", "group_manage") => Some("group:manage"),
        ("tools/call", "subscription_list" | "subscription_inspect") | ("events/list", _) => Some("subscription:read"),
        ("tools/call", "subscription_cancel" | "subscription_label_edit")
        | ("events/subscribe" | "events/unsubscribe", _) => Some("subscription:write"),
        _ => None,
    };
    needed
        .and_then(|needed| insufficient(state, needed))
        .unwrap_or((200, json!({})))
}

/// REST takes the current plain Bearer of the base audience, never a proof.
pub(super) fn rest(request: &Request, state: &mut State) -> (u16, Value) {
    let bearer = state
        .endpoint
        .access
        .map(|access| format!("Bearer endpoint-access-{access}"));
    if state.revoked || bearer.is_none() || request.headers.get("authorization") != bearer.as_ref() {
        return (401, json!({}));
    }
    if request.headers.contains_key("dpop") {
        return (401, json!({}));
    }
    let (method, needed) = match request.path.as_str() {
        "/plugin-auth-code-exchanges" => ("POST", "endpoint:write"),
        "/routes" => ("GET", "endpoint:read"),
        _ => ("POST", "message:send"),
    };
    if request.method != method {
        return (405, json!({}));
    }
    if let Some(refused) = insufficient(state, needed) {
        return refused;
    }
    match request.path.as_str() {
        "/plugin-auth-code-exchanges" => exchange(request, state),
        "/routes" => routes(request, state),
        _ => messages(request, state),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Exchange {
    principal_id: String,
    plugin_id: String,
    plugin_authentication_data: Identity,
    #[serde(default)]
    auth_link_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Identity {
    device_id: String,
    device_name: String,
    #[serde(default)]
    model: Option<String>,
}

fn bounded(text: &str, max: usize) -> bool {
    (1..=max).contains(&text.chars().count())
}

fn exchange(request: &Request, state: &mut State) -> (u16, Value) {
    let Some(body) = request.json.clone() else {
        return (400, json!({}));
    };
    let Ok(install) = serde_json::from_value::<Exchange>(body) else {
        return (400, json!({}));
    };
    let identity = &install.plugin_authentication_data;
    if install.principal_id != "~self"
        || install.plugin_id != PLUGIN_ID
        || identity.device_id.trim().is_empty()
        || !bounded(&identity.device_name, 64)
        || identity.model.as_deref().is_some_and(|model| !bounded(model, 64))
        || install.auth_link_id.as_deref().is_some_and(|link| link != AUTH_LINK)
    {
        return (400, json!({}));
    }
    let Some(key) = request.headers.get("idempotency-key") else {
        return (400, json!({}));
    };
    let renewal = install.auth_link_id.is_some();
    if let Some(prior) = state.endpoint.exchanges.get(key) {
        if prior != &request.json {
            return (409, json!({}));
        }
    } else if !state.endpoint.exchanges.is_empty() && !renewal {
        return (409, json!({}));
    }
    state.endpoint.device_id = Some(identity.device_id.clone());
    state.endpoint.exchanges.insert(key.clone(), request.json.clone());
    (
        200,
        json!({"authLinkId":AUTH_LINK,"id":"synthetic-install","etag":"synthetic-etag","status":"SUCCEEDED"}),
    )
}

/// One page of the caller's routes: `maxResults` (default 10, at most 100)
/// and an opaque `paginationContext.nextToken` that must come back unchanged
/// as the `nextToken` query parameter.
fn routes(request: &Request, state: &mut State) -> (u16, Value) {
    if request.query.get("owner").map(String::as_str) != Some("~caller") {
        return (400, json!({}));
    }
    let size = match request.query.get("maxResults").map(|size| size.parse::<usize>()) {
        None => ROUTE_PAGE,
        Some(Ok(size)) if (1..=MAX_ROUTE_PAGE).contains(&size) => size,
        Some(_) => return (400, json!({})),
    };
    let start = match request.query.get("nextToken") {
        None => 0,
        Some(_) if state.endpoint.route_token_rejections > 0 => {
            state.endpoint.route_token_rejections -= 1;
            return (400, json!({}));
        }
        Some(token) => match token
            .strip_prefix(ROUTE_TOKEN)
            .and_then(|start| start.parse::<usize>().ok())
        {
            Some(start) => start,
            None => return (400, json!({})),
        },
    };
    let all = owner_routes(state, start == 0);
    let end = all.len().min(start.saturating_add(size));
    let results = all.get(start..end).unwrap_or_default();
    let mut page = json!({"results":results});
    if end < all.len() {
        page["paginationContext"] = json!({"nextToken":format!("{ROUTE_TOKEN}{end}")});
    }
    (200, page)
}

/// An old pairing's route for the Echo, the padding routes, another device,
/// then the Echo once discovery caught up.
fn owner_routes(state: &mut State, first_page: bool) -> Vec<Value> {
    let other = |index: usize, device: &str| {
        json!({"id":format!("synthetic-other-route-{index}"),"associatedPluginRouteId":device,
        "associatedEndpointId":"synthetic-other-endpoint","associatedAuthLinkId":"synthetic-other-link","externalId":""})
    };
    let old_pairing = state.endpoint.device_id.as_ref().zip(state.endpoint.stale_link);
    let mut results: Vec<_> = old_pairing
        .map(|(device, link)| {
            json!({"id":"synthetic-stale-route","associatedPluginRouteId":device,
            "associatedEndpointId":STALE_ENDPOINT_ID,"associatedAuthLinkId":link,"externalId":""})
        })
        .into_iter()
        .chain((0..state.endpoint.route_padding).map(|index| other(index, &format!("synthetic-padding-{index}"))))
        .collect();
    results.push(other(state.endpoint.route_padding, OTHER_DEVICE));
    if state.endpoint.route_delay > 0 {
        if first_page {
            state.endpoint.route_delay -= 1;
        }
    } else if let Some(device) = &state.endpoint.device_id {
        results.push(json!({"id":"synthetic-echo-route","associatedPluginRouteId":device,
            "associatedEndpointId":ECHO_ENDPOINT_ID,"associatedAuthLinkId":AUTH_LINK,"externalId":""}));
    }
    results
}

fn request_id(request: &Request) -> Option<&str> {
    request
        .json
        .as_ref()
        .and_then(|body| body["payload"]["requestId"].as_str())
}

fn messages(request: &Request, state: &mut State) -> (u16, Value) {
    let claimed = request
        .headers
        .get("authorization")
        .and_then(|bearer| bearer.strip_prefix("Bearer "))
        .is_some_and(|token| state.endpoint.claimed.contains(token));
    if !claimed {
        return (401, json!({}));
    }
    let Some(key) = request.headers.get("idempotency-key").cloned() else {
        return (400, json!({"code":"BAD_REQUEST"}));
    };
    if state.endpoint.report_keys.contains(&key) {
        return (202, json!({}));
    }
    // A report without `requestId` is plain state. One with it must answer an
    // open command, once; with no command waiting (a speaker ack after its
    // caller gave up) Port OS answers 404.
    if request_id(request).is_some_and(|id| !state.endpoint.open_requests.contains(id)) {
        state.endpoint.refusals.push("requestId answers no open command".into());
        return (404, json!({"code":"NOT_FOUND"}));
    }
    // Port OS has no route for the device until discovery lists it.
    if request_id(request).is_none() && state.endpoint.route_delay > 0 {
        return (404, json!({"code":"NOT_FOUND"}));
    }
    let now_ms = wall_now(state)
        .ok()
        .and_then(|now| i64::try_from(now).ok())
        .map_or(0, |now| now.saturating_mul(1000));
    let device = state.endpoint.device_id.clone().unwrap_or_default();
    match request
        .json
        .as_ref()
        .ok_or("not JSON")
        .and_then(|body| envelope(body, &device, now_ms))
    {
        Ok(()) => {
            if let Some(id) = request_id(request) {
                state.endpoint.open_requests.remove(id);
            }
            state.endpoint.report_keys.insert(key);
            (202, json!({}))
        }
        Err(reason) => {
            state.endpoint.refusals.push(reason.into());
            (400, json!({"code":"BAD_REQUEST"}))
        }
    }
}
