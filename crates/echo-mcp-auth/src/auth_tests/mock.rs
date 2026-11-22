use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use p256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::JoinHandle,
    time::Instant,
};

#[path = "mock_http/mod.rs"]
mod http;
#[path = "mock_portos/mod.rs"]
mod portos;
pub use portos::{
    AUTH_LINK, ECHO_ENDPOINT_ID, EndpointState, OTHER_DEVICE, ROUTE_TOKEN, STALE_ENDPOINT_ID, STALE_LINK,
};
#[path = "mock_proofs.rs"]
#[cfg(test)]
mod tests;

#[derive(Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    /// The last value of each form field.
    pub form: BTreeMap<String, String>,
    /// Every form field in order, repeats included.
    pub form_all: Vec<(String, String)>,
    pub query: BTreeMap<String, String>,
    pub json: Option<Value>,
    pub time: u64,
    pub received: Instant,
    pub message_body: Option<Arc<[u8]>>,
    pub status: Option<u16>,
}

pub enum MessageFault {
    Disconnect,
    Status(u16),
}

#[derive(Default)]
pub struct State {
    epoch: u64,
    #[cfg(test)]
    pub(crate) raw_wall: Option<u64>,
    pub requests: Vec<Request>,
    pub polling: VecDeque<String>,
    pub generation: u64,
    pub revoked: bool,
    pub bound: bool,
    pub key: Option<Value>,
    /// The key of the device authorization whose approval is still awaited.
    pub pending: Option<Value>,
    /// Every key that has started a device authorization.
    pub linked: BTreeSet<String>,
    pub jtis: BTreeSet<String>,
    pub lifetime: u64,
    pub fault: Option<(String, u16, Value)>,
    pub message_faults: BTreeMap<String, VecDeque<MessageFault>>,
    pub metadata: Option<Value>,
    pub hold: Option<String>,
    pub held: bool,
    mcp_access: u64,
    pub endpoint: EndpointState,
}

pub struct Mock {
    pub base: String,
    pub state: Arc<Mutex<State>>,
    pub release: Arc<Notify>,
    task: JoinHandle<()>,
}

impl Mock {
    #[cfg(test)]
    pub(crate) async fn start() -> Self {
        Self::bind().await.unwrap()
    }

    /// Bind one reusable loopback server with the original fixture policy.
    /// # Errors
    /// Returns a local bind failure without creating a server task.
    pub async fn bind() -> std::io::Result<Self> {
        Self::bind_at_epoch(wall_now(&State::default())?).await
    }

    /// Bind with an independent epoch shared by simulated clients across restarts.
    /// # Errors
    /// Returns a local bind failure without creating a server task.
    pub async fn bind_at_epoch(epoch: u64) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("{}:{}", "http://127.0.0.1", listener.local_addr()?.port());
        let state = Arc::new(Mutex::new(State {
            epoch,
            lifetime: 3600,
            ..State::default()
        }));
        let release = Arc::new(Notify::new());
        let task = tokio::spawn(serve(listener, base.clone(), state.clone(), release.clone()));
        Ok(Self {
            base,
            state,
            release,
            task,
        })
    }

    /// Allow the same synthetic refresh grant to mint a second audience.
    /// # Errors
    /// Reports an unavailable mock state.
    pub fn enable_endpoint_audience(&self) -> std::io::Result<()> {
        let mut state = self.state.lock().map_err(|_| invalid())?;
        state.endpoint.enabled = true;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_epoch(&self, epoch: u64) {
        let mut state = self.state.lock().unwrap();
        state.epoch = epoch;
        state.raw_wall = Some(epoch);
    }

    #[cfg(test)]
    pub(crate) fn count(&self, path: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r.path == path)
            .count()
    }

    #[cfg(test)]
    pub(crate) fn refreshes(&self) -> usize {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r.form.get("grant_type").is_some_and(|v| v == "refresh_token"))
            .count()
    }

    #[cfg(test)]
    pub(crate) fn fault(&self, path: &str, status: u16, body: Value) {
        self.state.lock().unwrap().fault = Some((path.into(), status, body));
    }

    #[cfg(test)]
    pub(crate) async fn resource(&self, access: &str) -> u16 {
        reqwest::Client::new()
            .post(format!("{}/mcp", self.base))
            .bearer_auth(access)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    }
}

impl State {
    /// The device gateway admits a current access token of either audience
    /// that carries the auth-link claim; it closes any other with 4403.
    #[must_use]
    pub fn admits_gateway(&self, bearer: &str) -> bool {
        portos::admits_gateway(self, bearer)
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(listener: TcpListener, base: String, state: Arc<Mutex<State>>, release: Arc<Notify>) {
    let start = Instant::now();
    while let Ok((stream, _)) = listener.accept().await {
        // Each request is handled by this reusable root task; no process or
        // server per exchange, and no detached per-request workers.
        let _ = handle(stream, &base, &state, &release, start).await;
    }
}

fn wall_now(state: &State) -> std::io::Result<u64> {
    #[cfg(test)]
    if let Some(now) = state.raw_wall {
        return Ok(now);
    }
    let _ = state;
    crate::wire::unix_now().map_err(|_| invalid())
}

fn invalid() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid mock request")
}

async fn handle(
    mut stream: TcpStream,
    base: &str,
    shared: &Mutex<State>,
    release: &Notify,
    start: Instant,
) -> std::io::Result<()> {
    let mut request = http::read(&mut stream, start).await?;
    let (status, body, hold) = {
        let mut state = shared.lock().map_err(|_| invalid())?;
        let hold = state.hold.as_ref() == Some(&request.path);
        if hold {
            state.held = true;
        }
        let (status, body) = route(&request, base, &mut state);
        let status = message_status(&mut state, &request, status);
        request.status = status;
        state.requests.push(request);
        (status, body, hold)
    };
    let Some(status) = status else { return Ok(()) };
    if hold {
        release.notified().await;
    }
    let body = body.to_string();
    let challenge = if status == 403 {
        "WWW-Authenticate: Bearer error=\"insufficient_scope\"\r\n"
    } else {
        ""
    };
    let response = format!(
        "HTTP/1.1 {status} mock\r\nContent-Type: application/json\r\n{challenge}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await
}

fn message_status(state: &mut State, request: &Request, status: u16) -> Option<u16> {
    if request.path != "/messages" || status != 202 {
        return Some(status);
    }
    let fault = request
        .json
        .as_ref()
        .and_then(|body| body["payload"]["requestId"].as_str())
        .and_then(|id| state.message_faults.get_mut(id))
        .and_then(VecDeque::pop_front);
    match fault {
        Some(MessageFault::Disconnect) => None,
        Some(MessageFault::Status(status)) => Some(status),
        None => Some(status),
    }
}

fn route(request: &Request, base: &str, state: &mut State) -> (u16, Value) {
    portos::observe_registration(request, state);
    if let Some((path, status, body)) = &state.fault
        && path == &request.path
    {
        return (*status, body.clone());
    }
    match request.path.as_str() {
        "/.well-known/oauth-protected-resource" => (
            200,
            json!({"resource":format!("{base}/mcp"),"authorization_servers":[base]}),
        ),
        "/.well-known/oauth-authorization-server" => (200, state.metadata.clone().unwrap_or_else(|| metadata(base))),
        "/custom/link" | "/custom/token" => oauth(request, base, state),
        "/custom/end" => {
            if request.form.get("token") == Some(&format!("refresh-{}", state.generation)) {
                state.revoked = true;
            }
            (200, json!({}))
        }
        "/mcp" => {
            let allowed = !state.revoked
                && state.bound
                && state.mcp_access != 0
                && request.headers.get("authorization") == Some(&format!("Bearer access-{}", state.mcp_access));
            if !allowed {
                (401, json!({}))
            } else if state.endpoint.enabled {
                portos::tool_scope(request, state)
            } else {
                (200, json!({}))
            }
        }
        "/plugin-auth-code-exchanges" | "/messages" | "/routes" if state.endpoint.enabled => {
            portos::rest(request, state)
        }
        _ => (404, json!({})),
    }
}

fn oauth(request: &Request, base: &str, state: &mut State) -> (u16, Value) {
    if !verify(request, base, state) {
        return fail("invalid_dpop_proof");
    }
    if request.path == "/custom/link" {
        device(request, base, state)
    } else {
        token(request, base, state)
    }
}

#[must_use]
pub fn metadata(base: &str) -> Value {
    json!({"issuer":base,"device_authorization_endpoint":format!("{base}/custom/link"),
        "token_endpoint":format!("{base}/custom/token"),"revocation_endpoint":format!("{base}/custom/end"),
        "dpop_signing_alg_values_supported":["ES256"]})
}

fn fail(name: &str) -> (u16, Value) {
    (400, json!({"error":name}))
}

fn device(request: &Request, base: &str, state: &mut State) -> (u16, Value) {
    let resource = request.form.get("resource");
    if state.endpoint.enabled {
        if let Err(error) = portos::authorize(request, base, state) {
            return fail(error);
        }
    } else if resource.is_some_and(|r| r != &format!("{base}/mcp")) {
        return fail("invalid_target");
    }
    let Some(jwk) = proof_key(request) else {
        return fail("invalid_dpop_proof");
    };
    if !state.linked.insert(jwk.to_string()) {
        return fail("invalid_dpop_proof");
    }
    state.pending = Some(jwk);
    state.bound = resource.is_some();
    state.revoked = false;
    (
        200,
        json!({"device_code":"synthetic-device","user_code":"TEST-CODE","verification_uri":format!("{base}/approve"),"expires_in":1800,"interval":5}),
    )
}

fn token(request: &Request, base: &str, state: &mut State) -> (u16, Value) {
    if state.revoked {
        return fail("invalid_grant");
    }
    let refresh = request.form.get("grant_type").is_some_and(|g| g == "refresh_token");
    let endpoint = match portos::audience(request, base, state, refresh) {
        Ok(endpoint) => endpoint,
        Err(error) => return fail(error),
    };
    if refresh {
        if request.form.get("refresh_token") != Some(&format!("refresh-{}", state.generation)) {
            state.revoked = true;
            return fail("invalid_grant");
        }
    } else if let Some(error) = state.polling.pop_front() {
        return fail(&error);
    }
    state.generation += 1;
    // Only a refresh after pairing mints the auth-link claim.
    let installed = refresh && !state.endpoint.exchanges.is_empty();
    let audience = if endpoint {
        state.endpoint.access = Some(state.generation);
        state.endpoint.endpoint_claimed = installed;
        "endpoint-"
    } else {
        state.mcp_access = state.generation;
        state.endpoint.mcp_claimed = installed;
        ""
    };
    let access = format!("{audience}access-{}", state.generation);
    if installed {
        state.endpoint.claimed.insert(access.clone());
    }
    let scope = if state.endpoint.enabled {
        state.endpoint.scope.clone()
    } else {
        "endpoint:read".into()
    };
    (
        200,
        json!({"access_token":access,"refresh_token":format!("refresh-{}",state.generation),
        "token_type":"Bearer","expires_in":state.lifetime,"scope":scope}),
    )
}

fn verify(request: &Request, base: &str, state: &mut State) -> bool {
    let Some(proof) = request.headers.get("dpop") else {
        return false;
    };
    let parts: Vec<_> = proof.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    let decode = |part: &str| -> Option<Value> { serde_json::from_slice(&B64.decode(part).ok()?).ok() };
    let Some(header) = decode(parts[0]) else {
        return false;
    };
    let Some(claims) = decode(parts[1]) else {
        return false;
    };
    if header["typ"] != "dpop+jwt"
        || header["alg"] != "ES256"
        || claims["htm"] != "POST"
        || claims["htu"] != format!("{base}{}", request.path)
        || claims["ath"] != Value::Null
        || claims["iat"]
            .as_u64()
            .is_none_or(|iat| iat.abs_diff(state.epoch.saturating_add(request.time)) > 2)
    {
        return false;
    }
    let jwk = &header["jwk"];
    if jwk["d"] != Value::Null || jwk["kty"] != "EC" || jwk["crv"] != "P-256" {
        return false;
    }
    let Some(key) = verifying_key(jwk) else {
        return false;
    };
    let signature = B64.decode(parts[2]).ok().and_then(|b| Signature::from_slice(&b).ok());
    if signature.is_none_or(|sig| {
        key.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
            .is_err()
    }) {
        return false;
    }
    let Some(jti) = claims["jti"].as_str() else {
        return false;
    };
    bound(request, jwk, state) && state.jtis.insert(jti.into())
}

// Port OS accepts one device authorization per key. Its device-code exchange
// must use that key, which then binds the grant for every refresh.
fn bound(request: &Request, jwk: &Value, state: &mut State) -> bool {
    if request.path == "/custom/link" {
        // `device` records the key once the request itself is acceptable.
        return true;
    }
    let exchange = request
        .form
        .get("grant_type")
        .is_some_and(|grant| grant.ends_with("device_code"));
    let expected = if exchange { state.pending.as_ref() } else { None };
    if expected.or(state.key.as_ref()).is_some_and(|key| key != jwk) {
        return false;
    }
    state.key = Some(jwk.clone());
    true
}

fn proof_key(request: &Request) -> Option<Value> {
    let header = request.headers.get("dpop")?.split('.').next()?;
    let header: Value = serde_json::from_slice(&B64.decode(header).ok()?).ok()?;
    Some(header["jwk"].clone())
}

fn verifying_key(jwk: &Value) -> Option<VerifyingKey> {
    let mut point = vec![4];
    point.extend(B64.decode(jwk["x"].as_str()?).ok()?);
    point.extend(B64.decode(jwk["y"].as_str()?).ok()?);
    VerifyingKey::from_sec1_bytes(&point).ok()
}
