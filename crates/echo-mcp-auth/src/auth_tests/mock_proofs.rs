//! T1 mock policy witnesses: forged or malformed proofs must be refused.
use super::*;
use crate::proof;
use p256::ecdsa::{SigningKey, signature::Signer};

fn request(header: &Value, claims: &Value, key: &SigningKey) -> Request {
    let input = format!("{}.{}", B64.encode(header.to_string()), B64.encode(claims.to_string()));
    let signature: Signature = key.sign(input.as_bytes());
    Request {
        method: "POST".into(),
        path: "/custom/token".into(),
        headers: BTreeMap::from([("dpop".into(), format!("{input}.{}", B64.encode(signature.to_bytes())))]),
        form: BTreeMap::new(),
        form_all: Vec::new(),
        query: BTreeMap::new(),
        json: None,
        time: 0,
        received: Instant::now(),
        message_body: None,
        status: None,
    }
}

#[test]
fn mock_refuses_invalid_claims_headers_signature_keys_and_reused_jti() {
    let private = proof::generate();
    let jwk = private.expose();
    let key = proof::signing_key(jwk).unwrap();
    let header = json!({"typ":"dpop+jwt","alg":"ES256","jwk":{"kty":jwk.kty,"crv":jwk.crv,"x":jwk.x,"y":jwk.y}});
    let claims =
        json!({"htm":"POST","htu":"http://127.0.0.1/custom/token","iat":1_800_000_000_u64,"jti":"synthetic-id"});
    for field in ["htm", "htu", "ath", "iat", "jti"] {
        let mut changed = claims.clone();
        changed[field] = if field == "iat" {
            json!(0)
        } else if field == "jti" {
            Value::Null
        } else {
            json!("invalid")
        };
        assert!(!verify(
            &request(&header, &changed, key.expose()),
            "http://127.0.0.1",
            &mut fixture_state()
        ));
    }
    for field in ["typ", "alg"] {
        let mut changed = header.clone();
        changed[field] = json!("invalid");
        assert!(!verify(
            &request(&changed, &claims, key.expose()),
            "http://127.0.0.1",
            &mut fixture_state()
        ));
    }
    for field in ["kty", "crv", "x", "y", "d"] {
        let mut changed = header.clone();
        changed["jwk"][field] = json!("invalid");
        assert!(!verify(
            &request(&changed, &claims, key.expose()),
            "http://127.0.0.1",
            &mut fixture_state()
        ));
    }
    let mut state = fixture_state();
    let valid = request(&header, &claims, key.expose());
    assert!(verify(&valid, "http://127.0.0.1", &mut state));
    assert!(!verify(&valid, "http://127.0.0.1", &mut state));
    let other = proof::generate();
    let mut changed = header.clone();
    changed["jwk"] = json!({"kty":"EC","crv":"P-256","x":other.expose().x,"y":other.expose().y});
    let mut next = claims.clone();
    next["jti"] = json!("another-id");
    let other_key = proof::signing_key(other.expose()).unwrap();
    assert!(!verify(
        &request(&changed, &next, other_key.expose()),
        "http://127.0.0.1",
        &mut state
    ));
    assert!(!verify(
        &request(&header, &next, other_key.expose()),
        "http://127.0.0.1",
        &mut fixture_state()
    ));
}

#[test]
fn mock_refuses_truncated_or_missing_proofs() {
    for proof in [
        None,
        Some("bad"),
        Some("not-base64.e30.bad"),
        Some("e30.not-base64.bad"),
        Some("e30.e30.bad"),
    ] {
        let request = Request {
            method: "POST".into(),
            path: "/custom/token".into(),
            headers: proof.into_iter().map(|p| ("dpop".into(), p.into())).collect(),
            form: BTreeMap::new(),
            form_all: Vec::new(),
            query: BTreeMap::new(),
            json: None,
            time: 0,
            received: Instant::now(),
            message_body: None,
            status: None,
        };
        assert!(!verify(&request, "http://127.0.0.1", &mut fixture_state()));
    }
}

fn fixture_state() -> State {
    State {
        epoch: 1_800_000_000,
        ..State::default()
    }
}

#[test]
fn mock_iat_window_is_inclusive_and_tracks_fake_elapsed_time() {
    let private = proof::generate();
    let jwk = private.expose();
    let key = proof::signing_key(jwk).unwrap();
    let header = json!({"typ":"dpop+jwt","alg":"ES256","jwk":{"kty":jwk.kty,"crv":jwk.crv,"x":jwk.x,"y":jwk.y}});
    for (iat, accepted) in [
        (1_799_999_997, false),
        (1_799_999_998, true),
        (1_800_000_000, true),
        (1_800_000_002, true),
        (1_800_000_003, false),
    ] {
        let claims = json!({"htm":"POST","htu":"http://127.0.0.1/custom/token","iat":iat,"jti":"window-id"});
        let proof = request(&header, &claims, key.expose());
        assert_eq!(verify(&proof, "http://127.0.0.1", &mut fixture_state()), accepted);
        let mut elapsed = proof;
        elapsed.time = 10;
        assert!(!verify(&elapsed, "http://127.0.0.1", &mut fixture_state()));
    }
    for iat in [Value::Null, json!("invalid")] {
        let claims = json!({"htm":"POST","htu":"http://127.0.0.1/custom/token","iat":iat,"jti":"invalid-iat"});
        assert!(!verify(
            &request(&header, &claims, key.expose()),
            "http://127.0.0.1",
            &mut fixture_state()
        ));
    }
}
