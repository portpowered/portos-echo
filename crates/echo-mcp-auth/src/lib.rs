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

pub use auth::{AuthClient, GrantStatus, LinkInstructions};
use echo_contracts::secret::Secret;
use std::{future::Future, path::PathBuf, pin::Pin, time::Duration};

pub type TokenFuture<'a> = Pin<Box<dyn Future<Output = Result<Secret<String>, AuthError>> + Send + 'a>>;

/// Consumer seam for MCP: access is plain Bearer, without a `DPoP` proof.
pub trait TokenProvider: Send + Sync {
    fn access_token(&self) -> TokenFuture<'_>;
}

/// The storage directory must be outside the repository and owned by this user.
pub struct AuthOptions<I = String> {
    pub resource_url: String,
    pub client_id: I,
    pub scopes: Vec<String>,
    pub storage_dir: PathBuf,
    pub request_timeout: Duration,
}

/// Select a pre-registered public client or register one on explicit link.
pub enum LinkClient {
    Explicit(String),
    Automatic { name: String },
}

impl AuthOptions {
    fn explicit(self) -> AuthOptions<LinkClient> {
        AuthOptions {
            resource_url: self.resource_url,
            client_id: LinkClient::Explicit(self.client_id),
            scopes: self.scopes,
            storage_dir: self.storage_dir,
            request_timeout: self.request_timeout,
        }
    }
}

impl AuthOptions<LinkClient> {
    pub(crate) fn validate(&self) -> Result<(), AuthError> {
        let text = match &self.client_id {
            LinkClient::Explicit(id) => id,
            LinkClient::Automatic { name } => name,
        };
        if !valid_text(text) || self.request_timeout.is_zero() {
            return Err(AuthError::Discovery);
        }
        Ok(())
    }
}

pub(crate) fn valid_text(text: &str) -> bool {
    !text.trim().is_empty() && !text.chars().any(char::is_control)
}

/// Fixed diagnostic categories deliberately omit response bodies and URLs.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("OAuth discovery failed")]
    Discovery,
    #[error("Port OS registration is unavailable. Set mcp.client_id to use a registered client.")]
    RegistrationUnavailable,
    #[error("Port OS refused client registration (invalid_client_metadata).")]
    RegistrationRefused,
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
