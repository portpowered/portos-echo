//! Trust for the endpoint's REST client and gateway: native roots plus the
//! bridge's configured bundle (`codex.ca_file`), the one the Port OS grant and
//! the MCP client already trust. The Echo has no native roots at all, so
//! without the bundle every endpoint TLS handshake fails.
use rustls::{
    ClientConfig, RootCertStore,
    pki_types::{CertificateDer, pem::PemObject},
};
use std::sync::Arc;
use tokio_tungstenite::Connector;

/// The configured roots in the two forms the endpoint clients take.
pub(super) struct Trust {
    pub(super) rest: Vec<reqwest::Certificate>,
    pub(super) gateway: Option<Connector>,
}

impl Trust {
    /// `None` refuses a malformed or empty bundle. Without one, both clients keep
    /// their native roots.
    pub(super) fn configured(ca_pem: Option<&[u8]>) -> Option<Self> {
        let Some(pem) = ca_pem else {
            return Some(Self {
                rest: Vec::new(),
                gateway: None,
            });
        };
        let rest = reqwest::Certificate::from_pem_bundle(pem).ok()?;
        let extra = CertificateDer::pem_slice_iter(pem)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        if rest.is_empty() || extra.is_empty() {
            return None;
        }
        let mut roots = RootCertStore::empty();
        let (added, _) = roots.add_parsable_certificates(extra);
        if added == 0 {
            return None;
        }
        roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .ok()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Some(Self {
            rest,
            gateway: Some(Connector::Rustls(Arc::new(config))),
        })
    }
}

/// A fixed reason for a gateway upgrade that did not reach an HTTP answer.
pub(super) fn unanswered(error: &tokio_tungstenite::tungstenite::Error) -> &'static str {
    use std::io::ErrorKind;
    use tokio_tungstenite::tungstenite::Error;
    match error {
        // tokio-tungstenite reports a refused handshake as `InvalidData` and
        // an empty native root store as `NotFound`.
        Error::Io(io) if matches!(io.kind(), ErrorKind::InvalidData | ErrorKind::NotFound) => "tls",
        Error::Tls(_) => "tls",
        Error::Io(_) => "connect",
        Error::Http(_) => "http",
        _ => "protocol",
    }
}
