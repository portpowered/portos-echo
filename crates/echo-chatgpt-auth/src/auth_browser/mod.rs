mod auth_callback;

use crate::{AuthError, AuthSource, auth_http, auth_store::Record, auth_token};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use echo_contracts::secret::Secret;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::net::TcpListener;

struct Proof {
    verifier: Secret<String>,
    state: Secret<String>,
}
fn random() -> Result<Secret<String>, AuthError> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| AuthError::Transport)?;
    Ok(Secret::new(URL_SAFE_NO_PAD.encode(bytes)))
}
fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}
fn authorization(source: &AuthSource, proof: &Proof, redirect: &str) -> Result<String, AuthError> {
    let mut url = reqwest::Url::parse(&source.config.authorization_endpoint).map_err(|_| AuthError::InvalidConfig)?;
    url.query_pairs_mut().extend_pairs([
        ("response_type", "code"),
        ("client_id", &source.config.client_id),
        ("redirect_uri", redirect),
        ("scope", &source.config.scopes),
        ("code_challenge", &challenge(proof.verifier.expose())),
        ("code_challenge_method", "S256"),
        ("state", proof.state.expose()),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", "echo-bridge"),
    ]);
    Ok(url.into())
}
pub(crate) async fn sign_in(source: &AuthSource, callback: impl FnOnce(String)) -> Result<Record, AuthError> {
    let mut redirect =
        reqwest::Url::parse(&source.config.browser_redirect_uri).map_err(|_| AuthError::InvalidConfig)?;
    let port = redirect.port_or_known_default().ok_or(AuthError::InvalidConfig)?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|_| AuthError::Busy)?;
    #[cfg(test)]
    let listener = {
        let listener = std::sync::Arc::new(listener);
        *source.browser_listener.lock().unwrap() = std::sync::Arc::downgrade(&listener);
        listener
    };
    if port == 0 {
        redirect
            .set_port(Some(listener.local_addr().map_err(|_| AuthError::Transport)?.port()))
            .map_err(|()| AuthError::InvalidConfig)?;
    }
    let proof = Proof {
        verifier: random()?,
        state: random()?,
    };
    callback(authorization(source, &proof, redirect.as_str())?);
    let code = tokio::time::timeout(
        Duration::from_mins(15),
        auth_callback::receive(&listener, proof.state.expose()),
    )
    .await
    .map_err(|_| AuthError::Timeout)??;
    let request = source.client.post(&source.config.token_endpoint).form(&[
        ("grant_type", "authorization_code"),
        ("client_id", &source.config.client_id),
        ("code", code.expose()),
        ("code_verifier", proof.verifier.expose()),
        ("redirect_uri", redirect.as_str()),
    ]);
    let (status, body) = auth_http::request(request, Duration::from_secs(30)).await?;
    if !status.is_success() {
        return Err(AuthError::RelinkRequired);
    }
    auth_token::response(&body, source.clock.unix()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rfc7636_s256_vector_and_independent_randomness() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let a = random().unwrap();
        let b = random().unwrap();
        assert_eq!(a.expose().len(), 43);
        assert_ne!(a.expose(), b.expose());
    }
}
