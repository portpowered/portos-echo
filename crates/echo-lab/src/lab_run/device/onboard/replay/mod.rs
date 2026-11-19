//! Read-only actor for one consumed refresh token; the bridge remains the grant writer.
#[cfg(test)]
mod tests;
mod transport;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use echo_contracts::secret::Secret;
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use serde_json::{Value, json};

use crate::lab_run::device::{onboard::device_path, runtime::Runtime};

const ERROR: &str = "device: inconsistent MCP replay evidence";

#[derive(Default)]
pub(in crate::lab_run::device) struct Replay {
    initial: Option<Secret<Vec<u8>>>,
    attempted: bool,
}

impl Runtime {
    pub(in crate::lab_run::device) fn capture_mcp_replay(&mut self, config: &str) -> Result<(), String> {
        if self.replay.initial.is_none() {
            let remote = self.remote.as_ref().ok_or(ERROR)?;
            self.replay.initial = Some(remote.mcp_grant(config)?);
        }
        Ok(())
    }

    pub(in crate::lab_run::device) async fn mcp_replay(&mut self, config: &str, python: &str) -> Result<(), String> {
        let result = self.replay_inner(config, python).await;
        self.invalid |= result.is_err();
        result
    }

    async fn replay_inner(&mut self, config: &str, python: &str) -> Result<(), String> {
        if self.pending_mcp.is_some() || self.replay.attempted || self.replay.initial.is_none() {
            return Err(ERROR.into());
        }
        self.observe()?;
        let counts = self.portos.as_ref().ok_or(ERROR)?.measure()?;
        if counts.link != 1 || counts.forced_refresh != 2 || counts.tool_call != 1 || counts.restart_grant_kept != 1 {
            return Err(ERROR.into());
        }
        let remote = self.remote.as_ref().ok_or(ERROR)?;
        let current = remote.mcp_grant(config)?;
        let initial = self.replay.initial.as_ref().ok_or(ERROR)?;
        let record = rotated(initial, &current, config)?;
        let parsed = echo_config::BridgeConfig::from_toml_str(config).map_err(|_| ERROR)?;
        let path = parsed
            .codex
            .ca_file
            .as_deref()
            .and_then(std::path::Path::to_str)
            .ok_or(ERROR)?;
        if !device_path(path) {
            return Err(ERROR.into());
        }
        let pem = remote.read(&format!("head -c 1048577 {path}")).map_err(|_| ERROR)?;
        if pem.is_empty() || pem.len() > 1_048_576 {
            return Err(ERROR.into());
        }
        // Set before the first request: cancellation or uncertainty must never retry a token.
        self.replay.attempted = true;
        reject(python, &record, &pem).await?;
        if remote.mcp_grant(config)?.expose() != current.expose() {
            return Err(ERROR.into());
        }
        self.portos.as_mut().ok_or(ERROR)?.replay_rejected()?;
        Ok(())
    }
}

fn rotated(initial: &Secret<Vec<u8>>, current: &Secret<Vec<u8>>, config: &str) -> Result<Secret<Value>, String> {
    let initial: Secret<Value> = Secret::new(serde_json::from_slice(initial.expose()).map_err(|_| ERROR)?);
    let current: Secret<Value> = Secret::new(serde_json::from_slice(current.expose()).map_err(|_| ERROR)?);
    let (old, new) = (initial.expose(), current.expose());
    let config = echo_config::BridgeConfig::from_toml_str(config).map_err(|_| ERROR)?;
    let [server] = config.mcp.server.as_slice() else {
        return Err(ERROR.into());
    };
    if old["version"] != 1
        || new["version"] != 1
        || old["resource"] != server.url
        || old["client_id"] != config.mcp.client_id
        || ["issuer", "resource", "client_id", "private_key_jwk"]
            .iter()
            .any(|key| old[key] != new[key])
        || old["grant"]["refresh_token"].as_str().is_none_or(str::is_empty)
        || new["grant"]["refresh_token"].as_str().is_none_or(str::is_empty)
        || old["grant"]["refresh_token"] == new["grant"]["refresh_token"]
    {
        return Err(ERROR.into());
    }
    https(old["issuer"].as_str().ok_or(ERROR)?)?;
    Ok(initial)
}

fn https(value: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(value).map_err(|_| ERROR)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ERROR.into());
    }
    Ok(url)
}

async fn reject(python: &str, record: &Secret<Value>, pem: &[u8]) -> Result<(), String> {
    let record = record.expose();
    let issuer = record["issuer"].as_str().ok_or(ERROR)?;
    let metadata = format!(
        "{}/.well-known/oauth-authorization-server",
        issuer.trim_end_matches('/')
    );
    let (status, discovery) = transport::request(python, "GET", &metadata, "", "", pem).await?;
    if status != 200 || discovery.expose()["issuer"] != issuer {
        return Err(ERROR.into());
    }
    let endpoint = discovery.expose()["token_endpoint"].as_str().ok_or(ERROR)?;
    if https(endpoint)?.origin() != https(issuer)?.origin() {
        return Err(ERROR.into());
    }
    let proof = proof(record, endpoint)?;
    let body = Secret::new(
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "refresh_token")
            .append_pair("client_id", record["client_id"].as_str().ok_or(ERROR)?)
            .append_pair("refresh_token", record["grant"]["refresh_token"].as_str().ok_or(ERROR)?)
            .finish(),
    );
    let (status, value) = transport::request(python, "POST", endpoint, proof.expose(), body.expose(), pem).await?;
    if status != 400 || value.expose()["error"] != "invalid_grant" {
        return Err(ERROR.into());
    }
    Ok(())
}

fn proof(record: &Value, endpoint: &str) -> Result<Secret<String>, String> {
    let jwk = &record["private_key_jwk"];
    let private = Secret::new(B64.decode(jwk["d"].as_str().ok_or(ERROR)?).map_err(|_| ERROR)?);
    let key = Secret::new(SigningKey::from_slice(private.expose()).map_err(|_| ERROR)?);
    let public = key.expose().verifying_key().to_sec1_point(false);
    if jwk["kty"] != "EC"
        || jwk["crv"] != "P-256"
        || jwk["x"] != B64.encode(&public.as_bytes()[1..33])
        || jwk["y"] != B64.encode(&public.as_bytes()[33..65])
    {
        return Err(ERROR.into());
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| ERROR)?;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let header = json!({"typ":"dpop+jwt","alg":"ES256",
        "jwk":{"kty":"EC","crv":"P-256","x":jwk["x"],"y":jwk["y"]}});
    let claims = json!({"htm":"POST","htu":endpoint,"iat":now.as_secs(),
        "jti":format!("m5-{}-{}-{unique}",std::process::id(),now.as_nanos())});
    let input = format!("{}.{}", B64.encode(header.to_string()), B64.encode(claims.to_string()));
    let signature: Signature = key.expose().sign(input.as_bytes());
    Ok(Secret::new(format!("{input}.{}", B64.encode(signature.to_bytes()))))
}
