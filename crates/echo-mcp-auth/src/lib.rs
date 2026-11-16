//! Port OS device linking. Credentials remain on the bridge host.
//!
//! One `AuthClient` owns a storage directory; share it through `Arc` rather than
//! opening concurrent writers. Discovery and linking require explicit calls.
//! A failed refresh after server rotation requires recovery/relinking; it is
//! never retried automatically with the previous refresh credential.

mod auth;
mod discovery;
mod proof;
mod store;
mod wire;

/// Loopback-only authorization fixture; never enabled by the production bridge.
#[cfg(any(test, feature = "mock"))]
#[path = "auth_tests/mock.rs"]
pub mod mock;

pub use auth::{AuthClient, LinkInstructions};
use echo_contracts::secret::Secret;
use std::{future::Future, path::PathBuf, pin::Pin, time::Duration};

pub type TokenFuture<'a> = Pin<Box<dyn Future<Output = Result<Secret<String>, AuthError>> + Send + 'a>>;

/// Consumer seam for MCP: access is plain Bearer, without a `DPoP` proof.
pub trait TokenProvider: Send + Sync {
    fn access_token(&self) -> TokenFuture<'_>;
}

/// The storage directory must be outside the repository and owned by this user.
pub struct AuthOptions {
    pub resource_url: String,
    pub client_id: String,
    pub scopes: Vec<String>,
    pub storage_dir: PathBuf,
    pub request_timeout: Duration,
}

impl AuthOptions {
    pub(crate) fn validate(&self) -> Result<(), AuthError> {
        if self.client_id.is_empty() || self.request_timeout.is_zero() {
            return Err(AuthError::Discovery);
        }
        Ok(())
    }
}

/// Fixed diagnostic categories deliberately omit response bodies and URLs.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("OAuth discovery failed")]
    Discovery,
    #[error("OAuth transport failed; operation may be incomplete")]
    Transport,
    #[error("OAuth protocol failed")]
    Protocol,
    #[error("Link approval denied")]
    AccessDenied,
    #[error("Link code expired")]
    Expired,
    #[error("Grant requires relinking")]
    RelinkRequired,
    #[error("Grant storage failed; rotation may require recovery")]
    Storage,
}
