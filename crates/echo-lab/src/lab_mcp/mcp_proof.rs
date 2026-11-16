use super::State;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use p256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use serde_json::Value;

pub(super) fn decode(token: &str) -> Option<(Value, Value, String, Signature)> {
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 3 || token.len() > 8192 {
        return None;
    }
    let header = serde_json::from_slice(&B64.decode(parts[0]).ok()?).ok()?;
    let claims = serde_json::from_slice(&B64.decode(parts[1]).ok()?).ok()?;
    let signature = Signature::from_slice(&B64.decode(parts[2]).ok()?).ok()?;
    Some((header, claims, format!("{}.{}", parts[0], parts[1]), signature))
}

pub(super) fn verify(state: &mut State, proof: &str, path: &str, register: bool) -> bool {
    let Some((header, claims, input, signature)) = decode(proof) else {
        return false;
    };
    let jwk = &header["jwk"];
    if header["typ"] != "dpop+jwt"
        || header["alg"] != "ES256"
        || jwk["kty"] != "EC"
        || jwk["crv"] != "P-256"
        || jwk.get("d").is_some()
        || claims["htm"] != "POST"
        || claims["htu"] != format!("{}{path}", state.issuer)
        || claims.get("ath").is_some()
        || claims["iat"].as_u64().is_none_or(|iat| iat.abs_diff(state.now) > 2)
    {
        return false;
    }
    let Some(key) = verifying_key(jwk) else {
        return false;
    };
    if key.verify(input.as_bytes(), &signature).is_err() || (!register && state.device_key.as_ref() != Some(jwk)) {
        return false;
    }
    let Some(jti) = claims["jti"].as_str().filter(|value| !value.is_empty()) else {
        return false;
    };
    if state.jtis.len() >= 4096 || !state.jtis.insert(jti.into()) {
        return false;
    }
    if register {
        state.device_key = Some(jwk.clone());
    }
    true
}

fn verifying_key(jwk: &Value) -> Option<VerifyingKey> {
    let x = B64.decode(jwk["x"].as_str()?).ok()?;
    let y = B64.decode(jwk["y"].as_str()?).ok()?;
    if x.len() != 32 || y.len() != 32 {
        return None;
    }
    let mut point = vec![4];
    point.extend(x);
    point.extend(y);
    VerifyingKey::from_sec1_bytes(&point).ok()
}
