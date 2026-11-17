use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use p256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::JoinHandle,
    time::Instant,
};

#[path = "mock_proofs.rs"]
#[cfg(test)]
mod tests;

#[derive(Clone)]
pub struct Request {
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub form: BTreeMap<String, String>,
    pub time: u64,
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
    pub jtis: BTreeSet<String>,
    pub lifetime: u64,
    pub fault: Option<(String, u16, Value)>,
    pub metadata: Option<Value>,
    pub hold: Option<String>,
    pub held: bool,
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
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("{}:{}", "http://127.0.0.1", listener.local_addr()?.port());
        let state = Arc::new(Mutex::new(State {
            epoch: wall_now(&State::default())?,
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

async fn read(stream: &mut TcpStream, start: Instant) -> std::io::Result<Request> {
    let mut bytes = Vec::new();
    let end = loop {
        let mut byte = [0];
        stream.read_exact(&mut byte).await?;
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            break bytes.len();
        }
        if bytes.len() >= 16384 {
            return Err(invalid());
        }
    };
    let text = String::from_utf8(bytes).map_err(|_| invalid())?;
    let mut lines = text.split("\r\n");
    let path = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(invalid)?
        .into();
    let headers: BTreeMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.to_lowercase(), v.trim().into()))
        .collect();
    let length = headers
        .get("content-length")
        .map_or(Ok(0), |v| v.parse::<usize>().map_err(|_| invalid()))?;
    if length > 65536 {
        return Err(invalid());
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await?;
    if end >= 16384 {
        return Err(invalid());
    }
    Ok(Request {
        path,
        headers,
        form: url::form_urlencoded::parse(&body).into_owned().collect(),
        time: start.elapsed().as_secs(),
    })
}

async fn handle(
    mut stream: TcpStream,
    base: &str,
    shared: &Mutex<State>,
    release: &Notify,
    start: Instant,
) -> std::io::Result<()> {
    let request = read(&mut stream, start).await?;
    let (status, body, hold) = {
        let mut state = shared.lock().map_err(|_| invalid())?;
        state.requests.push(request.clone());
        let hold = state.hold.as_ref() == Some(&request.path);
        if hold {
            state.held = true;
        }
        let (status, body) = route(&request, base, &mut state);
        (status, body, hold)
    };
    if hold {
        release.notified().await;
    }
    let body = body.to_string();
    let response = format!(
        "HTTP/1.1 {status} mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await
}

fn route(request: &Request, base: &str, state: &mut State) -> (u16, Value) {
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
        "/custom/link" | "/custom/token" => {
            if !verify(request, base, state) {
                return fail("invalid_dpop_proof");
            }
            if request.path == "/custom/link" {
                device(request, base, state)
            } else {
                token(request, state)
            }
        }
        "/custom/end" => {
            if request.form.get("token") == Some(&format!("refresh-{}", state.generation)) {
                state.revoked = true;
            }
            (200, json!({}))
        }
        "/mcp" => {
            let allowed = !state.revoked
                && state.bound
                && request.headers.get("authorization") == Some(&format!("Bearer access-{}", state.generation));
            (if allowed { 200 } else { 401 }, json!({}))
        }
        _ => (404, json!({})),
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
    if resource.is_some_and(|r| r != &format!("{base}/mcp")) {
        return fail("invalid_target");
    }
    state.bound = resource.is_some();
    state.revoked = false;
    (
        200,
        json!({"device_code":"synthetic-device","user_code":"TEST-CODE","verification_uri":format!("{base}/approve"),"expires_in":1800,"interval":5}),
    )
}

fn token(request: &Request, state: &mut State) -> (u16, Value) {
    if state.revoked {
        return fail("invalid_grant");
    }
    if request.form.get("grant_type").is_some_and(|g| g == "refresh_token") {
        if request.form.get("refresh_token") != Some(&format!("refresh-{}", state.generation)) {
            state.revoked = true;
            return fail("invalid_grant");
        }
    } else if let Some(error) = state.polling.pop_front() {
        return fail(&error);
    }
    state.generation += 1;
    (
        200,
        json!({"access_token":format!("access-{}",state.generation),"refresh_token":format!("refresh-{}",state.generation),
        "token_type":"Bearer","expires_in":state.lifetime,"scope":"endpoint:read"}),
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
    if state.key.as_ref().is_some_and(|key| key != jwk) {
        return false;
    }
    let Some(jti) = claims["jti"].as_str() else {
        return false;
    };
    if !state.jtis.insert(jti.into()) {
        return false;
    }
    state.key = Some(jwk.clone());
    true
}

fn verifying_key(jwk: &Value) -> Option<VerifyingKey> {
    let mut point = vec![4];
    point.extend(B64.decode(jwk["x"].as_str()?).ok()?);
    point.extend(B64.decode(jwk["y"].as_str()?).ok()?);
    VerifyingKey::from_sec1_bytes(&point).ok()
}
