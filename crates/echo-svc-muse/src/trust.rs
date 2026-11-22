use ring::digest;
use rustls::{
    ClientConfig, RootCertStore,
    pki_types::{CertificateDer, pem::PemObject},
};
use x509_parser::parse_x509_certificate;

use crate::ServiceError;

// These DER and SPKI pairs are the compromised roots refused by Muse's SDK.
const BLOCKED_ROOTS: [(&str, &str); 2] = [
    (
        "86a1ecba089c4a8d3bbe2734c612ba341d813e043cf9e8a862cd5c57a36bbe6b",
        "8d417db2dd8bf5e3084d1e3f196d583849d81bdd4c00c70b9d39369e96b8c782",
    ),
    (
        "d7a7a0fb5d7e2731d771e9484ebcdef71d5f0c3e0a2948782bc83ee0ea699ef4",
        "bd153ed7b0434f6886b17bce8bbe84ed340c7132d702a8f4fa318f756ecbd6f3",
    ),
];

pub(super) fn client_config(pem: &[u8]) -> Result<ClientConfig, ServiceError> {
    let roots = load_roots(pem, &BLOCKED_ROOTS)?;
    Ok(
        ClientConfig::builder_with_provider(rustls::crypto::ring::default_provider().into())
            .with_safe_default_protocol_versions()
            .map_err(|_| ServiceError::TrustUnavailable)?
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

fn load_roots(pem: &[u8], blocked: &[(&str, &str)]) -> Result<RootCertStore, ServiceError> {
    let mut store = RootCertStore::empty();
    for der in CertificateDer::pem_slice_iter(pem) {
        let der = der.map_err(|_| ServiceError::TrustUnavailable)?;
        let (certificate, spki) = fingerprints(&der)?;
        if blocked
            .iter()
            .any(|(bad_cert, bad_key)| certificate == *bad_cert || spki == *bad_key)
        {
            return Err(ServiceError::TrustUnavailable);
        }
        store.add(der).map_err(|_| ServiceError::TrustUnavailable)?;
    }
    if store.is_empty() {
        return Err(ServiceError::TrustUnavailable);
    }
    Ok(store)
}

fn fingerprints(der: &[u8]) -> Result<(String, String), ServiceError> {
    let (rest, certificate) = parse_x509_certificate(der).map_err(|_| ServiceError::TrustUnavailable)?;
    if !rest.is_empty() {
        return Err(ServiceError::TrustUnavailable);
    }
    Ok((sha256(der), sha256(certificate.public_key().raw)))
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(digest::digest(&digest::SHA256, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};

    fn certificate(key: &KeyPair, name: &str) -> (Vec<u8>, String, String) {
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.distinguished_name.push(DnType::CommonName, name);
        let cert = params.self_signed(key).unwrap();
        let (cert_hash, key_hash) = fingerprints(cert.der()).unwrap();
        let pem = format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            STANDARD.encode(cert.der())
        );
        (pem.into_bytes(), cert_hash, key_hash)
    }

    #[test]
    fn t1_trust_refuses_each_blocked_slot_by_der_or_reissued_spki() {
        for index in 0..BLOCKED_ROOTS.len() {
            let key = KeyPair::generate().unwrap();
            let (pem, cert, spki) = certificate(&key, "test root");
            let (reissued, new_cert, new_spki) = certificate(&key, "test root reissued");
            assert_ne!(cert, new_cert);
            assert_eq!(spki, new_spki);
            let mut blocked = BLOCKED_ROOTS;
            blocked[index].0 = &cert;
            assert_eq!(load_roots(&pem, &blocked).unwrap_err(), ServiceError::TrustUnavailable);
            assert_eq!(load_roots(&reissued, &blocked).unwrap().len(), 1);
            blocked[index].1 = &spki;
            assert_eq!(
                load_roots(&reissued, &blocked).unwrap_err(),
                ServiceError::TrustUnavailable
            );
            assert_eq!(load_roots(&pem, &BLOCKED_ROOTS).unwrap().len(), 1);
        }
    }

    #[test]
    fn t1_trust_invalid_or_empty_certificates_fail_closed() {
        for data in [
            b"".as_slice(),
            b"not PEM",
            b"-----BEGIN CERTIFICATE-----\nYWJj\n-----END CERTIFICATE-----\n",
        ] {
            assert!(matches!(client_config(data), Err(ServiceError::TrustUnavailable)));
        }
    }
}
