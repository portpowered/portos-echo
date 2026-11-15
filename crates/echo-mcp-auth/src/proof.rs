use crate::AuthError;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use echo_contracts::secret::Secret;
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PrivateJwk {
    pub kty: String,
    pub crv: String,
    pub x: String,
    pub y: String,
    pub d: String,
}

pub(crate) fn generate() -> Secret<PrivateJwk> {
    let key = SigningKey::random(&mut OsRng);
    let point = key.verifying_key().to_encoded_point(false);
    let bytes = point.as_bytes();
    Secret::new(PrivateJwk {
        kty: "EC".into(),
        crv: "P-256".into(),
        x: B64.encode(&bytes[1..33]),
        y: B64.encode(&bytes[33..65]),
        d: B64.encode(key.to_bytes()),
    })
}

pub(crate) fn signing_key(jwk: &PrivateJwk) -> Result<Secret<SigningKey>, AuthError> {
    let bytes = B64.decode(&jwk.d).map_err(|_| AuthError::Storage)?;
    let key = SigningKey::from_slice(&bytes).map_err(|_| AuthError::Storage)?;
    let point = key.verifying_key().to_encoded_point(false);
    if jwk.kty != "EC"
        || jwk.crv != "P-256"
        || jwk.x != B64.encode(&point.as_bytes()[1..33])
        || jwk.y != B64.encode(&point.as_bytes()[33..65])
        || jwk.d != B64.encode(key.to_bytes())
    {
        return Err(AuthError::Storage);
    }
    Ok(Secret::new(key))
}

pub(crate) fn sign(jwk: &PrivateJwk, url: &str, now: u64) -> Result<Secret<String>, AuthError> {
    let key = signing_key(jwk)?;
    let header = json!({"typ":"dpop+jwt", "alg":"ES256", "jwk": {
        "kty":jwk.kty, "crv":jwk.crv, "x":jwk.x, "y":jwk.y
    }});
    let mut random = [0_u8; 32];
    OsRng.fill_bytes(&mut random);
    let claims = json!({"htm":"POST", "htu":url, "iat":now, "jti":B64.encode(random)});
    let input = format!("{}.{}", encode(&header)?, encode(&claims)?);
    let signature: Signature = key.expose().sign(input.as_bytes());
    Ok(Secret::new(format!("{input}.{}", B64.encode(signature.to_bytes()))))
}

fn encode(value: &serde_json::Value) -> Result<String, AuthError> {
    serde_json::to_vec(value)
        .map(|bytes| B64.encode(bytes))
        .map_err(|_| AuthError::Protocol)
}

#[cfg(test)]
#[path = "proof_tests.rs"]
mod tests;
