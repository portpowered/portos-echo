use super::{Shared, State, mcp_proof};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use echo_spoof_mcp::BearerValidator;
use p256::ecdsa::{
    Signature,
    signature::{Signer, Verifier},
};
use serde_json::{Value, json};

pub(super) struct Admission(pub Shared);

impl BearerValidator for Admission {
    fn protected_resource(&self) -> Option<Value> {
        let state = self.0.lock().ok()?;
        Some(json!({"resource":state.resource,"authorization_servers":[state.issuer]}))
    }

    fn accepts(&self, bearer: &str) -> bool {
        let Ok(state) = self.0.lock() else {
            return false;
        };
        let Some((header, claims, input, signature)) = mcp_proof::decode(bearer) else {
            return false;
        };
        header["alg"] == "ES256"
            && header["typ"] == "JWT"
            && state
                .key
                .expose()
                .verifying_key()
                .verify(input.as_bytes(), &signature)
                .is_ok()
            && valid_claims(&state, &claims)
    }
}

fn valid_claims(state: &State, claims: &Value) -> bool {
    !state.revoked
        && state.bound
        && state.generation != 0
        && claims["iss"] == state.issuer
        && claims["aud"] == state.resource
        && claims["token_use"] == "access"
        && claims["sub"] == "synthetic-principal"
        && claims["grant_id"] == format!("grant-{}", state.device)
        && claims["scope"] == state.scopes
        && claims["exp"].as_u64().is_some_and(|expiry| expiry > state.now)
        && claims["iat"].as_u64().is_some_and(|issued| issued <= state.now)
}

pub(super) fn sign(state: &State, claims: &Value) -> String {
    let header = json!({"typ":"JWT","alg":"ES256"});
    let input = format!("{}.{}", B64.encode(header.to_string()), B64.encode(claims.to_string()));
    let signature: Signature = state.key.expose().sign(input.as_bytes());
    format!("{input}.{}", B64.encode(signature.to_bytes()))
}

pub(super) fn pair(state: &mut State) -> Value {
    state.generation += 1;
    let claims = json!({"iss":state.issuer,"aud":if state.bound { state.resource.as_str() } else { "first-party" },
        "token_use":"access","sub":"synthetic-principal","grant_id":format!("grant-{}",state.device),
        "iat":state.now,"exp":state.now.saturating_add(3600),"scope":state.scopes});
    json!({"access_token":sign(state,&claims),"refresh_token":refresh(state),
        "token_type":"Bearer","expires_in":3600,"scope":state.scopes})
}

pub(super) fn refresh(state: &State) -> String {
    format!("synthetic-refresh-{}-{}", state.device, state.generation)
}
