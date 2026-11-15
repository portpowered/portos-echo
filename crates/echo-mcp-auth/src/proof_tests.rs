use super::*;
use p256::ecdsa::signature::Verifier;

#[test]
fn proofs_verify_and_keep_private_material_out_of_public_jwk() {
    let jwk = generate();
    let proof = sign(jwk.expose(), "https://example.invalid/token", 123).unwrap(); // lint-rules: allow-literal DPoP signing fixture encoded and verified locally without network access
    let parts: Vec<_> = proof.expose().split('.').collect();
    assert_eq!(parts.len(), 3);
    let header: serde_json::Value = serde_json::from_slice(&B64.decode(parts[0]).unwrap()).unwrap();
    let claims: serde_json::Value = serde_json::from_slice(&B64.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(header["typ"], "dpop+jwt");
    assert_eq!(header["alg"], "ES256");
    assert_eq!(header["jwk"].as_object().unwrap().len(), 4);
    assert_eq!(claims["htu"], "https://example.invalid/token"); // lint-rules: allow-literal DPoP signing fixture encoded and verified locally without network access
    assert_eq!(claims["htm"], "POST");
    assert_eq!(claims["iat"], 123);
    assert_eq!(claims.as_object().unwrap().len(), 4);
    let signature = Signature::from_slice(&B64.decode(parts[2]).unwrap()).unwrap();
    let key = signing_key(jwk.expose()).unwrap();
    key.expose()
        .verifying_key()
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .unwrap();
    let second = sign(jwk.expose(), "https://example.invalid/token", 123).unwrap(); // lint-rules: allow-literal DPoP signing fixture encoded and verified locally without network access
    let second_claims: serde_json::Value =
        serde_json::from_slice(&B64.decode(second.expose().split('.').nth(1).unwrap()).unwrap()).unwrap();
    assert_ne!(claims["jti"], second_claims["jti"]);
}

#[test]
fn private_keys_reject_corruption_and_mismatched_public_coordinates() {
    for field in ["kty", "crv", "x", "y", "d"] {
        let mut value = serde_json::to_value(generate().expose()).unwrap();
        value[field] = json!("invalid");
        let key: PrivateJwk = serde_json::from_value(value).unwrap();
        assert!(matches!(signing_key(&key), Err(AuthError::Storage)));
    }
}
